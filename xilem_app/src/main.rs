// Xilem (Rust native GUI) re-implementation of the image loader UI.
//
// 与 Makepad 版 (`src/app.rs` 等) 一比一对应的功能复刻：
//   - 顶部栏：打开文件夹 / 共: N张 / 幻灯片模式 / 退出
//   - 可拖拽分栏：左侧文件树、右侧内容区
//   - 内容区用 indexed_stack 切换：缩略图网格 (virtual_scroll) <-> 幻灯片浏览 (自定义 ImageView)
//   - 缩略图网格：每行若干卡片，卡片点击打开幻灯片
//   - 文件树：文件夹可展开/折叠，文件点击打开幻灯片
//   - 幻灯片：自定义 ImageView 支持光标锚定滚轮缩放、拖拽平移、双击切换、键盘 +/-/0、←/→/Esc
//   - 后台线程解码缩略图，由 worker（官方后台任务视图）接收 ready 通知并触发重绘
//
// 公共逻辑（扫描 / 解码 / 下采样 / 缓存）复用 `image_loader_core`。
// API 以 crates.io 发布版 xilem 0.4.0 / masonry 0.4.0 / ui-events 0.3.0 为准。

// 仅 release 构建隐藏控制台窗口；debug 构建（cargo run）保留控制台，便于查看诊断日志。
#![cfg_attr(all(not(test), not(debug_assertions)), windows_subsystem = "windows")]
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedSender;

use image_loader_core::cache::ThumbnailCache;
use image_loader_core::decode::decode_rgba8;
use image_loader_core::scanner::{FileTreeNode, ScanResult, scan_directory};
use image_loader_core::thumbnail::{THUMB_MAX_BITMAPS, THUMB_MAX_IN_FLIGHT};

use ui_events::keyboard::{Key, NamedKey};

use xilem::core::{
    MessageCtx, MessageProxy, MessageResult, Mut, NoElement, View, ViewMarker, fork,
};
use xilem::masonry::accesskit::{Node, Role};
use xilem::masonry::core::{
    AccessCtx, AccessEvent, ChildrenIds, EventCtx, LayoutCtx, MeasureCtx, PaintCtx, PointerButton,
    PointerEvent, PointerGesture, PointerUpdate, PropertiesMut, PropertiesRef, RegisterCtx,
    ScrollDelta, TextEvent, Widget, WidgetMut,
};
use xilem::masonry::dpi::{LogicalSize, PhysicalPosition};
use xilem::masonry::imaging::Painter;
use xilem::masonry::kurbo::{Affine, Axis, Point, Size, Vec2};
use xilem::masonry::layout::{AsUnit, Dim, LenReq, Length};
use xilem::masonry::peniko::{Color, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use xilem::masonry::properties::{Dimensions, Padding};
use xilem::style::Style as _;
use xilem::view::{
    AnyFlexChild, CrossAxisAlignment, FlexExt, FlexSpacer, ObjectFit, button, flex_col, flex_row,
    indexed_stack, label, resize_observer, sized_box, split, virtual_scroll, worker,
};
use xilem::{
    AnyWidgetView, EventLoop, FontWeight, Pod, TextAlign, ViewCtx, WidgetView, WindowOptions, Xilem,
};

// 主题色板（深色现代风）。
const APP_BG: Color = Color::from_rgb8(20, 22, 28);
const TOPBAR_BG: Color = Color::from_rgb8(26, 29, 37);
const SIDEBAR_BG: Color = Color::from_rgb8(24, 27, 34);
const CARD_BG: Color = Color::from_rgb8(34, 38, 48);
const IMG_BG: Color = Color::from_rgb8(14, 16, 21);
const STATUS_BG: Color = Color::from_rgb8(28, 31, 39);
const ARROW_BG: Color = Color::from_rgb8(38, 42, 53);
const NODE_BG: Color = Color::from_rgb8(30, 34, 43);
const ACCENT: Color = Color::from_rgb8(79, 126, 255);
const TEXT: Color = Color::from_rgb8(228, 231, 237);
const TEXT_MUTED: Color = Color::from_rgb8(150, 157, 170);
const BORDER: Color = Color::from_rgb8(46, 51, 62);

use rfd::FileDialog;

// 缩略图网格：列数随右侧内容区实际宽度自适应（窗口越宽列数越多，窗口越窄列数越少），
// 卡片本身用 `flex(1.0)` 等权均分整行宽度，因此任意窗口尺寸下都铺满整行、不会被截断。
//
// 自适应实现：用官方 `resize_observer` 视图包裹缩略图网格。它在**自身尺寸发生变化**时
// 才发出 `LayoutChanged` action（路由由框架保证），我们据此把真实宽度写回
// `AppState::pane_width` 并按真实宽度重算列数。宽度稳定后不再有 action，
// 因此不会陷入 action→rebuild→layout 的死循环。
// virtual_scroll 自身仍是普通视图（未被塞进自定义 widget），其 VirtualScrollAction 路由
// 完整保留，缩略图能正常加载、不会因 action 丢失而恐慌。
const TARGET_CARD_W: f64 = 180.0;
const GAP: f64 = 16.0;
// 卡片高度。
const CARD_H: f64 = 200.0;
// 卡片的边框宽度与内边距。卡片内容区尺寸 = 卡片尺寸 - 2*(边框+内边距)。
const CARD_BORDER: f64 = 1.0;
const CARD_PAD: f64 = 10.0;
const CARD_INSET: f64 = 2.0 * (CARD_BORDER + CARD_PAD);
// 每行左右内边距（与 row_view 中 flex_row 的横向 padding 保持一致）。
const ROW_PAD_X: f64 = 14.0;
// 列数上下限，避免极端窗口尺寸下产生不健康的列数（0 列或上百列）。
const MIN_COLS: usize = 1;
const MAX_COLS: usize = 16;
// 右侧滚动条槽位预留：masonry 的 virtual_scroll/Portal 竖向滚动条以 overlay 方式
// 画在内容最右缘（theme::SCROLLBAR_WIDTH 8 + SCROLLBAR_PAD 2*2 = 12px），不预留宽度，
// 会盖住最右列的右缘导致“右侧显示不完整”。这里在每行右内边距额外预留该槽位，
// 把最右列缩进、彻底避开滚动条。（多预留 4px 作为高分屏/主题的安全余量。）
const SCROLLBAR_GUTTER: f64 = 16.0;

// 侧栏目标宽度（与 Makepad 版 split 设 200px 对应；split_point 为比例值）。
const SIDEBAR_PX: f64 = 220.0;

// 缩放范围与步长（对应 Makepad 的 ZoomConfig）。
const ZOOM_MIN: f64 = 0.05;
const ZOOM_MAX: f64 = 25.0;
const ZOOM_STEP: f64 = 1.1;

// ---------------------------------------------------------------------------
// 应用状态
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageViewAction {
    NavigateLeft,
    NavigateRight,
    Exit,
}

#[derive(Clone, Debug)]
enum NodePayload {
    Folder(String),
    File(usize),
}

#[derive(Clone, Debug)]
struct FlatNode {
    depth: usize,
    name: String,
    is_folder: bool,
    expanded: bool,
    payload: NodePayload,
}

/// 后台线程解码全图后回传给主线程的结果。
/// `result` 为 `None` 表示解码失败（非图片/损坏）。
#[derive(Debug)]
struct FullResult {
    original_idx: usize,
    result: Option<(Vec<u8>, u32, u32)>,
}

/// 主线程 → 后台 worker 的请求。
///
/// 所有会阻塞/耗时的工作（目录扫描、整图解码、缩略图就绪转发）都通过这一个
/// `tokio::sync::mpsc` 异步通道交给 worker，worker 内部用 `spawn_blocking` 把真正的
/// 重活在独立线程上跑，再通过 `proxy.message` 把结果异步回传主线程。
/// 主线程从不直接做扫描/解码，因此 UI 永远不会因为大文件夹或大图而“未响应”。
#[derive(Debug)]
enum BgRequest {
    /// 让 worker 在后台线程打开文件夹选择对话框（rfd 在 Windows 上支持非主线程调用），
    /// 选中后立即在同线程扫描并回传 `ScanDone`；用户取消则回传 `PickCancelled`。
    /// 主线程从此没有任何模态对话框 / 阻塞调用。
    PickFolder,
    /// 直接扫描指定目录（诊断/自动化钩子 `IMG_AUTO_DIR` 使用）。
    Scan(std::path::PathBuf),
    Decode(usize, std::path::PathBuf),
    /// 由缩略图桥接线程转发：来自 `ThumbnailCache` 后台解码完成通知的索引。
    ThumbReady(usize),
}

/// 后台 worker → 主线程的回传结果，由 `on_response` 落到 `AppState`。
#[derive(Debug)]
enum BgResponse {
    ScanDone(ScanResult),
    DecodeDone(FullResult),
    ThumbReady(usize),
    /// 用户取消了文件夹选择，主线程恢复非加载态。
    PickCancelled,
}

struct AppState {
    image_paths: Vec<std::path::PathBuf>,
    filtered_image_idxs: Vec<usize>,
    current_image_idx: Option<usize>,
    current_original_idx: Option<usize>,
    in_slideshow: bool,
    file_tree_data: Option<image_loader_core::scanner::FileTreeData>,
    file_tree_open: bool,
    generation: u64,
    expanded_folders: std::collections::HashSet<String>,
    thumb_cache: std::sync::Arc<ThumbnailCache>,
    thumb_rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<usize>>>,
    thumb_brushes: std::collections::HashMap<usize, ImageBrush>,
    full_image: Option<ImageBrush>,
    full_image_size: Option<Size>,
    image_version: u64,
    // 后台 worker 的请求发送端：主线程通过它把“扫描目录 / 解码整图”请求发给 worker，
    // worker 在 `spawn_blocking` 线程里做重活，再用 `proxy.message` 把结果异步回传主线程。
    // 主线程从不直接扫描或解码，因此大文件夹 / 大图都不会卡死 UI。
    bg_tx: Option<UnboundedSender<BgRequest>>,
    // 目录扫描进行中标记：用于显示“正在加载文件夹…”占位，扫描完成即清除。
    scanning: bool,
    /// 右侧内容区（缩略图网格）实际可用宽度（像素）。由 `WidthReporter` 在布局时测量并回填，
    /// 用于动态计算网格列数，实现“窗口越宽、列数越多”的响应式自适应。
    pane_width: f64,
    /// 诊断钩子：设置了 `IMG_AUTO_DIR` 环境变量时，worker 建好后自动扫描该目录，
    /// 绕过文件对话框，便于无交互验证后台扫描链路。
    boot_scan: Option<std::path::PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    fn new() -> Self {
        let (cache, rx) = ThumbnailCache::new(THUMB_MAX_BITMAPS, THUMB_MAX_IN_FLIGHT);
        AppState {
            image_paths: Vec::new(),
            filtered_image_idxs: Vec::new(),
            current_image_idx: None,
            current_original_idx: None,
            in_slideshow: false,
            file_tree_data: None,
            file_tree_open: false,
            generation: 0,
            expanded_folders: std::collections::HashSet::new(),
            thumb_cache: cache,
            thumb_rx: Arc::new(Mutex::new(rx)),
            thumb_brushes: std::collections::HashMap::new(),
            full_image: None,
            full_image_size: None,
            image_version: 0,
            bg_tx: None,
            scanning: false,
            pane_width: 0.0,
            boot_scan: None,
        }
    }

    fn num_images(&self) -> usize {
        self.filtered_image_idxs.len()
    }

    fn num_rows(&self) -> usize {
        let cols = grid_cols(self.pane_width);
        self.num_images().div_ceil(cols)
    }

    fn recompute_filtered(&mut self, query: &str) {
        self.filtered_image_idxs.clear();
        for (i, p) in self.image_paths.iter().enumerate() {
            if p.to_string_lossy().contains(query) {
                self.filtered_image_idxs.push(i);
            }
        }
    }

    fn load_folder(&mut self) {
        // 主线程只负责清空旧数据并进入“加载中”状态，然后发一条 `PickFolder` 请求；
        // 文件夹选择对话框（模态）和目录扫描全部在后台 worker 线程里进行。
        // 之前主线程同步跑 rfd 模态对话框 + 同步扫描，模态消息泵会阻塞 Winit 事件循环，
        // 导致扫描结果回传后也无法被及时处理，表现为“永远正在加载/未响应”。
        self.image_paths.clear();
        self.filtered_image_idxs.clear();
        self.file_tree_data = None;
        self.file_tree_open = false;
        self.in_slideshow = false;
        self.full_image = None;
        self.full_image_size = None;
        self.current_image_idx = None;
        self.current_original_idx = None;
        self.thumb_brushes.clear();
        self.thumb_cache.clear();
        self.thumb_cache.bump_generation();
        self.generation = self.generation.wrapping_add(1);
        self.scanning = true;
        if let Some(tx) = &self.bg_tx {
            let _ = tx.send(BgRequest::PickFolder);
        }
    }

    /// 后台线程完成目录扫描后回传主线程填充 UI。
    fn on_scan_done(&mut self, scan: ScanResult) {
        eprintln!(
            "DBG[main] on_scan_done called, {} images",
            scan.image_paths.len()
        );
        self.image_paths = scan.image_paths;
        self.file_tree_data = Some(scan.tree);
        self.file_tree_open = true;
        self.expanded_folders.clear();
        if let Some(ref tree) = self.file_tree_data {
            self.expanded_folders
                .insert(tree.dir_path.to_string_lossy().to_string());
        }
        self.recompute_filtered("");
        self.scanning = false;
        self.generation = self.generation.wrapping_add(1);
        // 诊断钩子：IMG_AUTO_SLIDESHOW=1 时，扫描完成后自动进入幻灯片模式，
        // 便于用脚本发送按键来验证幻灯片的 action 路由与键盘操作。
        if std::env::var("IMG_AUTO_SLIDESHOW").is_ok() {
            self.open_slideshow();
        }
    }

    fn open_slideshow(&mut self) {
        eprintln!("DBG[slideshow] 进入幻灯片模式");
        self.in_slideshow = true;
        if self.current_image_idx.is_none() && !self.filtered_image_idxs.is_empty() {
            self.set_current_image(Some(0));
        }
    }

    fn exit_slideshow(&mut self) {
        eprintln!("DBG[slideshow] 退出幻灯片模式");
        self.in_slideshow = false;
    }

    /// 关闭当前文件夹：清空图片列表与文件树，回到空状态。
    /// （顶栏的「关闭文件夹」按钮；此前那个「退出」按钮实际调的是 exit_slideshow，
    /// 在非幻灯片模式下点了没有任何效果。）
    fn close_folder(&mut self) {
        eprintln!("DBG[main] 关闭当前文件夹");
        self.in_slideshow = false;
        self.image_paths.clear();
        self.filtered_image_idxs.clear();
        self.current_image_idx = None;
        self.current_original_idx = None;
        self.full_image = None;
        self.full_image_size = None;
        self.file_tree_data = None;
        self.file_tree_open = false;
        self.expanded_folders.clear();
        self.thumb_brushes.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    fn toggle_folder(&mut self, path: &str) {
        if !self.expanded_folders.remove(path) {
            self.expanded_folders.insert(path.to_string());
        }
    }

    fn navigate_left(&mut self) {
        if let Some(fi) = self.current_image_idx.filter(|&i| i > 0) {
            self.set_current_image(Some(fi - 1));
        }
    }

    fn navigate_right(&mut self) {
        if let Some(fi) = self
            .current_image_idx
            .filter(|&i| i + 1 < self.num_images())
        {
            self.set_current_image(Some(fi + 1));
        }
    }

    fn set_current_image(&mut self, filtered_idx: Option<usize>) {
        self.current_image_idx = filtered_idx;
        self.image_version += 1; // 重置缩放
        match filtered_idx {
            Some(fi) => {
                let original = self.filtered_image_idxs[fi];
                self.current_original_idx = Some(original);
                // 先清空，等待后台解码完成（避免看到上一张图闪一下）。
                self.full_image = None;
                self.full_image_size = None;
                // 异步解码，不阻塞 UI 线程；结果在 on_full_image_ready 里回填。
                self.load_full_image(original);
            }
            None => {
                self.current_original_idx = None;
                self.full_image = None;
                self.full_image_size = None;
            }
        }
    }

    fn open_image(&mut self, original_idx: usize) {
        if original_idx >= self.image_paths.len() {
            return;
        }
        if let Some(pos) = self
            .filtered_image_idxs
            .iter()
            .position(|&i| i == original_idx)
        {
            self.set_current_image(Some(pos));
        } else {
            // 不在当前过滤列表中的图片：直接用它原始索引打开。
            self.current_image_idx = None;
            self.current_original_idx = Some(original_idx);
            self.full_image = None;
            self.full_image_size = None;
            self.image_version += 1;
            self.load_full_image(original_idx);
        }
        self.in_slideshow = true;
    }

    /// 把整图解码请求发给后台 worker（`spawn_blocking` 线程里解码），主线程不阻塞。
    /// 即便是几十 MB 的大图也不会卡死 UI（之前是主线程同步解码，大图直接“未响应”）。
    fn load_full_image(&mut self, original_idx: usize) {
        if let Some(path) = self.image_paths.get(original_idx).cloned() {
            if let Some(tx) = &self.bg_tx {
                let _ = tx.send(BgRequest::Decode(original_idx, path));
            }
        }
    }

    /// 后台解码完成后由 bg_worker 调用：回填全图，但只采用“最新一次请求”的结果，
    /// 丢弃快速翻页时迟到的旧结果，避免画面跳回上一张。
    fn on_full_image_ready(&mut self, res: FullResult) {
        if Some(res.original_idx) != self.current_original_idx {
            return;
        }
        match res.result {
            Some((rgba, w, h)) => {
                let data = ImageData {
                    data: rgba.into(),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: w,
                    height: h,
                };
                self.full_image = Some(ImageBrush::new(data));
                self.full_image_size = Some(Size::new(w as f64, h as f64));
            }
            None => {
                self.full_image = None;
                self.full_image_size = None;
            }
        }
        self.image_version += 1;
    }

    fn ensure_thumbnail(&mut self, original_idx: usize) {
        if !self.thumb_brushes.contains_key(&original_idx) {
            if let Some(path) = self.image_paths.get(original_idx).cloned() {
                self.thumb_cache.request(original_idx, path);
            }
        }
    }

    fn on_thumbnail_ready(&mut self, idx: usize) {
        if let Some(bm) = self.thumb_cache.get(idx) {
            let rgba: Vec<u8> = bm.rgba.to_vec();
            let data = ImageData {
                data: rgba.into(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: bm.width as u32,
                height: bm.height as u32,
            };
            self.thumb_brushes.insert(idx, ImageBrush::new(data));
        }
    }

    fn flat_tree(&self) -> Vec<FlatNode> {
        let mut out = Vec::new();
        if let Some(ref tree) = self.file_tree_data {
            self.collect(&tree.root, 0, &mut out);
        }
        out
    }

    fn collect(&self, node: &FileTreeNode, depth: usize, out: &mut Vec<FlatNode>) {
        if let Some(image_idx) = node.image_idx {
            out.push(FlatNode {
                depth,
                name: node.name.clone(),
                is_folder: false,
                expanded: false,
                payload: NodePayload::File(image_idx),
            });
        } else {
            let expanded = self.expanded_folders.contains(&node.path);
            out.push(FlatNode {
                depth,
                name: node.name.clone(),
                is_folder: true,
                expanded,
                payload: NodePayload::Folder(node.path.clone()),
            });
            if expanded {
                for child in &node.children {
                    self.collect(child, depth + 1, out);
                }
            }
        }
    }
}

// --- 占位图（1x1 透明），缩略图未解码时显示 ---
static PLACEHOLDER: LazyLock<ImageBrush> = LazyLock::new(|| {
    let data = ImageData {
        data: vec![0u8; 4].into(),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 1,
        height: 1,
    };
    ImageBrush::new(data)
});

fn placeholder_brush() -> ImageBrush {
    PLACEHOLDER.clone()
}

// ---------------------------------------------------------------------------
// 视图
// ---------------------------------------------------------------------------

fn top_bar(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 顶栏右侧的按钮按当前状态动态决定是否出现，避免出现「点了没反应」的死按钮。
    let has_images = state.num_images() > 0;
    let has_folder = state.file_tree_data.is_some();
    let slideshow_button: Option<Box<AnyWidgetView<AppState>>> = if has_images {
        Some(
            button(
                label("幻灯片模式")
                    .text_size(13.0)
                    .weight(FontWeight::MEDIUM)
                    .color(TEXT),
                |s: &mut AppState| s.open_slideshow(),
            )
            .background_color(ARROW_BG)
            .corner_radius(8.0.px())
            .padding(Padding::from_vh(8.0.px(), 14.0.px()))
            .boxed(),
        )
    } else {
        None
    };
    let close_button: Option<Box<AnyWidgetView<AppState>>> = if has_folder {
        Some(
            button(
                label("关闭文件夹")
                    .text_size(13.0)
                    .weight(FontWeight::MEDIUM)
                    .color(TEXT),
                |s: &mut AppState| s.close_folder(),
            )
            .background_color(STATUS_BG)
            .corner_radius(8.0.px())
            .padding(Padding::from_vh(8.0.px(), 14.0.px()))
            .boxed(),
        )
    } else {
        None
    };
    flex_row((
        button(
            label("打开文件夹")
                .text_size(13.0)
                .weight(FontWeight::MEDIUM)
                .color(TEXT),
            |s: &mut AppState| s.load_folder(),
        )
        .background_color(ACCENT)
        .corner_radius(8.0.px())
        .padding(Padding::from_vh(8.0.px(), 14.0.px())),
        label(format!("共 {} 张", state.image_paths.len()))
            .text_size(13.0)
            .text_alignment(TextAlign::Left)
            .color(TEXT_MUTED),
        label("").flex(1.0),
        // 「幻灯片模式」只在确实有图片时才可用：没有图片时点了会进入一个空白的
        // 幻灯片界面，体验很差，因此此时不显示这个按钮。
        slideshow_button,
        // 关闭按钮的语义必须是「关闭当前文件夹」而不是「退出幻灯片」——
        // 后者在非幻灯片模式下点了没有任何效果，顶栏出现这样的按钮会让人困惑。
        close_button,
    ))
    // 注意：新版 xilem 里 cross_axis_alignment 是 Flex 结构体自身的方法，
    // 必须在 .gap()/.padding() 这类样式 Prop 之前调用。
    .cross_axis_alignment(CrossAxisAlignment::Center)
    .gap(Length::px(10.0))
    .background_color(TOPBAR_BG)
    .padding(Padding::from_vh(10.0.px(), 14.0.px()))
}

fn sidebar_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 诊断开关：NO_TREE=1 时文件树为空（隔离 virtual_scroll 死循环来源）。
    let no_tree = std::env::var("NO_TREE").is_ok();
    // 直接用空列表表达“无树”，避免 if/else 两分支返回不同类型。
    let flat: Vec<FlatNode> = if no_tree {
        Vec::new()
    } else {
        state.flat_tree()
    };
    let len = flat.len();
    // 还没有打开文件夹时，侧栏除了标题还要给一句说明，
    // 否则侧栏只有一个孤零零的「文件列表」标签，与右侧内容区比例失衡。
    // 还没有打开文件夹时，侧栏除了标题还要给一句说明，
    // 否则侧栏只有一个孤零零的「文件列表」标签，与右侧内容区比例失衡。
    let tree: Box<AnyWidgetView<AppState>> = if len == 0 {
        flex_col((
            label("尚未打开文件夹").text_size(12.0).color(TEXT_MUTED),
            label("打开后这里显示目录树")
                .text_size(11.0)
                .color(TEXT_MUTED),
        ))
        .gap(Length::px(4.0))
        .padding(Padding::from_vh(4.0.px(), 6.0.px()))
        .boxed()
    } else {
        virtual_scroll(len, move |s: &mut AppState, i: usize| node_row(s, i, &flat)).boxed()
    };
    flex_col((
        label("文件列表").text_size(11.0).color(TEXT_MUTED),
        tree.flex(1.0),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Stretch)
    .gap(Length::px(6.0))
    .background_color(SIDEBAR_BG)
    .padding(Padding::from_vh(10.0.px(), 6.0.px()))
    // 关键：virtual_scroll 必须显式占满剩余高度（flex(1.0)）。
    // 否则 flex(0) 让 VirtualScroll 拿到病态的高度约束，viewport/cutoff 计算异常，
    // 导致 masonry 每帧发出无法收敛的 VirtualScrollAction（大文件夹卡死）。
    // 顺带恢复树行按钮的 flex(1.0) 填满行宽。
}

fn build_split(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let sidebar = sidebar_view(state);
    let content = content_view(state);
    // 诊断开关：NO_TREE=1 时侧栏收起（配合 sidebar_view 内的空树）。
    let no_tree = std::env::var("NO_TREE").is_ok();
    // 用**固定像素宽度**而不是比例：侧栏宽度不再随窗口宽度变化，
    // 右侧内容区宽度 = 窗口宽 - 侧栏宽 - 分隔条，完全可预测、不会因比例换算而抖动。
    let sidebar_w = if state.file_tree_open && !no_tree {
        Length::px(SIDEBAR_PX)
    } else {
        Length::px(0.0)
    };
    split(sidebar, content)
        .split_axis(Axis::Horizontal)
        .split_point_from_start(sidebar_w)
        .draggable(true)
        .bar_thickness(Length::px(4.0))
        .min_bar_area(Length::px(4.0))
}

fn content_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 在「缩略图网格」与「幻灯片」之间切换。
    //
    // 这里用 `Option<...>` 序列而不是 indexed_stack：indexed_stack 会把非活动分支
    // 标记为 stashed，而 masonry 的 stashed 子树里若含 Flex，布局递归仍会进入它，
    // 触发 `trying to compute layout of stashed widget` 恐慌
    // （masonry_core/src/passes/layout.rs:327）。Option 为 None 时根本不构建该分支，
    // 因此彻底避开这个问题。背景由顶层 app_view 的 flex_col(APP_BG) 提供，此处透明即可。
    let slideshow_view = slideshow(state).boxed();
    let browser_view = image_browser(state).boxed();
    indexed_stack::<AppState, _, _>((slideshow_view, browser_view)).active(if state.in_slideshow {
        0
    } else {
        1
    })
}

fn image_browser(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 扫描后台进行时显示加载提示；扫描完成后切换到缩略图网格。
    let loading = flex_col((label("正在加载文件夹…").text_size(15.0).color(TEXT_MUTED),))
        .cross_axis_alignment(CrossAxisAlignment::Center);
    let nrows = state.num_rows().max(1);
    let grid = virtual_scroll(nrows, |s: &mut AppState, row_idx: usize| {
        row_view(s, row_idx)
    });
    // 用官方 resize_observer 测量网格所在区域的真实宽度，从而按真实宽度算列数
    // （窗口越宽列数越多）。它只在**自身尺寸发生变化**时才发出 LayoutChanged action，
    // 而 action 的路由由框架保证，因此“布局 → 状态更新 → 重建 → 布局”这条链会自然收敛：
    // 宽度稳定后不再有 action，不会出现之前那种 action→rebuild 死循环。
    // 观察器用 flex(1.0) 占满剩余高度，使它的高度与内容行数无关，进一步避免抖动。
    let observed = resize_observer(
        |s: &mut AppState, size: Size| {
            if (s.pane_width - size.width).abs() > 0.5 {
                let cols = grid_cols(size.width);
                let inner = (size.width - 2.0 * ROW_PAD_X - SCROLLBAR_GUTTER).max(0.0);
                let card_w = (inner - (cols.saturating_sub(1)) as f64 * GAP) / cols.max(1) as f64;
                eprintln!(
                    "DBG[grid] 内容区宽度 = {:.1}px → 列数 {} (每卡约 {:.1}px)",
                    size.width, cols, card_w
                );
                s.pane_width = size.width;
            }
        },
        grid,
    )
    .flex(1.0);
    // 空状态引导：尚未打开文件夹时显示在网格上方。
    //
    // 用 Option 作为 flex 子项（None 时该子项不存在），**不**给 indexed_stack 加第三分支：
    // masonry 的 IndexedStack 会把非活动分支标记为 stashed，而分��数变化时触发的
    // stashed 布局递归会引发 `trying to compute layout of stashed widget` 恐慌
    // （masonry_core/src/passes/layout.rs:327，实测两态稳定、三态必崩）。
    // Option 序列没有这个问题（顶栏的条件按钮就是用它实现的）。
    let empty_hint: Option<Box<AnyWidgetView<AppState>>> = if state.num_images() == 0 {
        Some(
            flex_col((
                // 用一个圆角方块作图标占位，纯样式实现，无需外部资源。
                sized_box(label("🖼").text_size(36.0).color(TEXT_MUTED))
                    .background_color(STATUS_BG)
                    .corner_radius(16.0.px())
                    .padding(Padding::from_vh(16.0.px(), 16.0.px())),
                label("还没有打开文件夹")
                    .text_size(17.0)
                    .weight(FontWeight::MEDIUM)
                    .color(TEXT),
                label("点击左上角「打开文件夹」浏览图片")
                    .text_size(13.0)
                    .color(TEXT_MUTED),
            ))
            // 注意：新版 xilem 里 cross_axis_alignment 是 Flex 结构体自身的方法，
            // 必须在 .gap()/.padding() 这类样式 Prop 之前调用。
            .cross_axis_alignment(CrossAxisAlignment::Center)
            .gap(Length::px(10.0))
            .padding(Padding::from_vh(28.0.px(), 24.0.px()))
            .boxed(),
        )
    } else {
        None
    };
    // 空状态时网格没有内容（0 行），提示占据整个区域并居中；
    // 有图片时提示消失，网格铺满。
    let content =
        flex_col((empty_hint, observed)).cross_axis_alignment(CrossAxisAlignment::Stretch);
    let loading_view = loading.boxed();
    let content_view = content.boxed();
    indexed_stack::<AppState, _, _>((loading_view, content_view)).active(if state.scanning {
        0
    } else {
        1
    })
}

fn slideshow(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let total = state.num_images();
    let cur = state.current_image_idx.map(|i| i + 1).unwrap_or(0);
    let name = state
        .current_image_idx
        .and_then(|fi| state.filtered_image_idxs.get(fi))
        .and_then(|&orig| state.image_paths.get(orig))
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();
    let info = if total == 0 {
        "没有图片".to_string()
    } else {
        format!("{}    {}/{}", name, cur, total)
    };
    // 整图还在后台解码时，先拿已经解码好的缩略图作即时占位，保证“点开即见、秒开”的体验；
    // 整图就绪后由 full_image 覆盖并重置缩放。
    let (disp_image, disp_size) = if let Some(b) = state.full_image.clone() {
        (Some(b), state.full_image_size)
    } else if let Some(orig) = state.current_original_idx {
        state
            .thumb_cache
            .get(orig)
            .map(|bm| {
                let data = ImageData {
                    data: bm.rgba.to_vec().into(),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: bm.width as u32,
                    height: bm.height as u32,
                };
                (
                    Some(ImageBrush::new(data)),
                    Some(Size::new(bm.width as f64, bm.height as f64)),
                )
            })
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    flex_row((
        sized_box(arrow_button("‹", |s: &mut AppState| s.navigate_left())).width(Length::px(56.0)),
        flex_col((
            sized_box(ImageView {
                image: disp_image,
                img_size: disp_size,
                version: state.image_version,
                fit: ObjectFit::Contain,
            })
            .flex(1.0),
            flex_row((
                label(info)
                    .text_size(12.0)
                    .text_alignment(TextAlign::Left)
                    .color(TEXT_MUTED)
                    .flex(1.0),
                button(
                    label("退出幻灯片")
                        .text_size(13.0)
                        .weight(FontWeight::MEDIUM)
                        .color(TEXT),
                    |s: &mut AppState| s.exit_slideshow(),
                )
                .background_color(ACCENT)
                .corner_radius(8.0.px())
                .border(BORDER, 1.0.px())
                .padding(Padding::from_vh(7.0.px(), 16.0.px())),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center)
            .gap(Length::px(12.0))
            .background_color(STATUS_BG)
            .padding(Padding::from_vh(8.0.px(), 12.0.px())),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Stretch)
        .flex(1.0),
        sized_box(arrow_button("›", |s: &mut AppState| s.navigate_right())).width(Length::px(56.0)),
    ))
    .background_color(APP_BG)
}

fn arrow_button(
    symbol: &'static str,
    action: fn(&mut AppState),
) -> impl WidgetView<AppState> + use<> {
    button(label(symbol).text_size(22.0).color(TEXT), action)
        .background_color(ARROW_BG)
        .corner_radius(10.0.px())
        .padding(Padding::from_vh(2.0.px(), 10.0.px()))
}

/// 根据右侧内容区实际可用宽度，计算网格列数（响应式自适应）：
/// 窗口越宽 → 列数越多，卡片保持接近 `TARGET_CARD_W` 的目标尺寸；
/// 窗口越窄 → 列数越少，卡片按比例缩小，但始终铺满整行、绝不被截断。
///
/// 推导：设行内可用宽度为 `inner`，则 `cols` 列需要 `cols*card_w + (cols-1)*GAP <= inner`，
/// 即 `cols <= (inner + GAP) / (TARGET_CARD_W + GAP)`，向下取整并 clamp 到 [MIN_COLS, MAX_COLS]。
///
/// `pane_width` 为 0（尚未测量）时给一个保守默认宽度，避免初始出现 0 列。
fn grid_cols(pane_width: f64) -> usize {
    let w = if pane_width > 1.0 { pane_width } else { 1000.0 };
    let cols = ((grid_inner_width(w) + GAP) / (TARGET_CARD_W + GAP)).floor() as usize;
    cols.clamp(MIN_COLS, MAX_COLS)
}

/// 内容区宽度扣除行内边距与滚动条槽位后，真正用于排布卡片的宽度。
fn grid_inner_width(pane_width: f64) -> f64 {
    let w = if pane_width > 1.0 { pane_width } else { 1000.0 };
    (w - 2.0 * ROW_PAD_X - SCROLLBAR_GUTTER).max(0.0)
}

/// 单张卡片的宽度：由内容区宽度与列数精确算出，保证
/// `cols * card_w + (cols-1) * GAP == grid_inner_width`，即整行**精确铺满**视口、既不溢出也不留白。
fn card_width(pane_width: f64, cols: usize) -> f64 {
    let inner = grid_inner_width(pane_width);
    let gaps = (cols.saturating_sub(1)) as f64 * GAP;
    ((inner - gaps) / cols.max(1) as f64).max(1.0)
}

fn row_view(state: &mut AppState, row_idx: usize) -> impl WidgetView<AppState> + use<> {
    let cols = grid_cols(state.pane_width);
    // 卡片宽度显式固定（而不是靠 flex 均分）。原因见下方 card_view 的注释：
    // masonry 的 Flex 在测主轴时 main_space 恒为 None，FitContent 会被降级成 MinContent，
    // flex_fraction 于是按子项的 MinContent（= 图片固有宽 490px）计算，
    // 整行测量宽度 = Σ 卡片 MinContent ≫ 视口宽 → 视口里只显示得下 1~2 张卡片。
    // 显式给出宽度后，行的测量宽度 = 精确的视口宽，卡片也就不再撑破卡片。
    let card_w = card_width(state.pane_width, cols);
    let first = row_idx * cols;
    let total = state.filtered_image_idxs.len();
    let count = cols.min(total.saturating_sub(first));
    // 行内子项数量固定为 cols：有图放卡片，空槽用固定宽度的占位，
    // 因此整行宽度 = cols*card_w + (cols-1)*GAP，精确等于内容区可用宽度。
    let mut children: Vec<AnyFlexChild<AppState>> = Vec::with_capacity(cols);
    for k in 0..cols {
        if k < count {
            let filtered_idx = first + k;
            let original_idx = state.filtered_image_idxs[filtered_idx];
            state.ensure_thumbnail(original_idx);
            let brush = state
                .thumb_brushes
                .get(&original_idx)
                .cloned()
                .unwrap_or_else(placeholder_brush);
            let name = state.image_paths[original_idx]
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            children.push(card_view(brush, name, original_idx, card_w));
        } else {
            children.push(FlexSpacer::Fixed(card_w.px()).into());
        }
    }
    flex_row(children)
        // 注意：新版 xilem 里 cross_axis_alignment 是 Flex 结构体自身的方法，
        // 必须在 .gap()/.padding() 这类样式 Prop 之前调用。
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .gap(Length::px(GAP))
        // 非对称内边距：左右各留 ROW_PAD_X，右侧额外加 SCROLLBAR_GUTTER 避开竖向滚动条
        // （masonry 的 Portal 滚动条 overlay 在最右缘，不预留宽度会盖住最右列）。
        .padding(Padding {
            left: Length::px(ROW_PAD_X),
            right: Length::px(ROW_PAD_X + SCROLLBAR_GUTTER),
            top: Length::px(8.0),
            bottom: Length::px(8.0),
        })
}

// ---------------------------------------------------------------------------
// 圆角缩略图 ThumbWidget
// ---------------------------------------------------------------------------
//
// 为什么需要自己写：masonry 自带的 `Image` 组件在 paint 里是
// `painter.with_fill_clip(content_box, ...)`——**只做矩形裁剪**；
// 而 `CornerRadius` 属性只影响**自身背景与边框**的绘制，**完全不裁剪子节点**
// （见 masonry_core/src/core/widget_paint.rs 与 passes/paint.rs）。
// 于是缩略图的 4 个直角会直接盖在卡片 14px 的圆角上，视觉上就像
// 「图片压在卡片上面」而不是「图片在卡片里面」。
//
// Makepad 版是靠给图片单独加 8px 圆角 mask 解决的（src/widgets/image_card.rs）。
// 这里用 masonry 的 Painter 做同样的事：把图片绘制到一个**圆角矩形**的裁剪区里，
// 从而让缩略图四角与卡片圆角语言一致。
//
// 另外这里用 `ObjectFit::Cover`（等价于 Makepad 的 ImageFit.CropToFill）：
// 图片铺满盒子、居中裁切，而不是 Contain 的留白——这样缩略图不会有黑边。

/// 缩略图圆角半径（与 Makepad 版 image_card.rs 的 8.0 保持一致）。
const THUMB_RADIUS: f64 = 8.0;

struct ThumbWidget {
    brush: ImageBrush,
    /// 图片的像素尺寸（用于计算 Cover 变换）。
    img_size: Size,
    /// 圆角半径（像素）。
    radius: f64,
}

impl ThumbWidget {
    fn new(brush: ImageBrush, img_size: Size, radius: f64) -> Self {
        Self {
            brush,
            img_size,
            radius,
        }
    }
}

impl Widget for ThumbWidget {
    type Action = xilem::masonry::core::NoAction;

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn measure(
        &mut self,
        _ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        _axis: Axis,
        _len_req: LenReq,
        _cross_length: Option<Length>,
    ) -> Length {
        // 缩略图**始终填满父级给的盒子**（父级是已算出确定尺寸的卡片）。
        //
        // 这里刻意返回 0 而不是图片固有尺寸（490px）：masonry 的 SizedBox 在没有显式
        // 尺寸时会采用子项的测量结果，一旦返回 490，图片盒高度就会暴涨到 490px，
        // 把 virtual_scroll 的行高从 ~200 撑到 490，行数减半 → 更多行落到视口外被
        // masonry stash，而 virtual_scroll 下轮仍会对已 stashed 的行 run_layout，
        // 递归进内部 Flex 后触发 `trying to compute layout of stashed widget` 恐慌
        // （masonry_core/src/passes/layout.rs:327）。
        //
        // 实际绘制时图片按 Cover 铺满 content_box，与固有尺寸无关。
        Length::px(0.0)
    }

    fn layout(&mut self, _ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, _size: Size) {}

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        painter: &mut xilem::masonry::imaging::Painter<'_>,
    ) {
        let content_box = ctx.content_box();
        if content_box.width() <= 0.0 || content_box.height() <= 0.0 {
            return;
        }
        // 用图片的**真实像素尺寸**计算变换（与 masonry Image::paint 一致），
        // 这样缩放比例才正确。
        let img_rect = self.img_size.to_rect();
        let transform = ObjectFit::Cover.affine(content_box, img_rect);
        // 关键：用圆角矩形做裁剪区，图片四角即为圆角。
        let rounded = content_box.to_rounded_rect(self.radius);
        painter.with_fill_clip(rounded, |painter| {
            painter.draw_image(&self.brush, transform);
        });
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut Node,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[])
    }

    fn accepts_pointer_interaction(&self) -> bool {
        false
    }
}

/// `ThumbWidget` 的 xilem 视图包装。
struct ThumbView {
    brush: ImageBrush,
    img_size: Size,
    radius: f64,
}

impl ViewMarker for ThumbView {}

impl View<AppState, (), ViewCtx> for ThumbView {
    type Element = Pod<ThumbWidget>;
    type ViewState = ();

    fn build(&self, ctx: &mut ViewCtx, _app_state: &mut AppState) -> (Self::Element, ()) {
        (
            ctx.create_pod(ThumbWidget::new(
                self.brush.clone(),
                self.img_size,
                self.radius,
            )),
            (),
        )
    }

    fn rebuild(
        &self,
        _prev: &Self,
        _vs: &mut Self::ViewState,
        _ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        _app_state: &mut AppState,
    ) {
        // 图片内容变化时更新绘制数据（缩略图解码完成会换 brush）。
        let w = &mut *element.widget;
        if w.brush != self.brush || w.img_size != self.img_size || w.radius != self.radius {
            w.brush = self.brush.clone();
            w.img_size = self.img_size;
            w.radius = self.radius;
            element.ctx.request_render();
        }
    }

    fn teardown(
        &self,
        _vs: &mut Self::ViewState,
        _ctx: &mut ViewCtx,
        _element: Mut<'_, Self::Element>,
    ) {
    }

    fn message(
        &self,
        _vs: &mut Self::ViewState,
        _message: &mut MessageCtx,
        _element: Mut<'_, Self::Element>,
        _app_state: &mut AppState,
    ) -> MessageResult<()> {
        MessageResult::Stale
    }
}

fn card_view(
    brush: ImageBrush,
    name: String,
    original_idx: usize,
    card_w: f64,
) -> AnyFlexChild<AppState> {
    // 图片的像素尺寸直接从 brush 取（ImageBrush.image 是公开字段），
    // 用于计算 Cover 变换的缩放比例。
    let img_size = Size::new(brush.image.width as f64, brush.image.height as f64);
    // 卡片内边距 10 + 边框 1；图片再往里缩 4px（对齐 Makepad 版 image_card.rs
    // 里 thumb 的 margin: 4.0），让缩略图与卡片边框之间有一圈可见的留白。
    let inner = flex_col((
        sized_box(
            ThumbView {
                brush,
                img_size,
                radius: THUMB_RADIUS,
            }
            .padding(Padding::from_vh(4.0.px(), 4.0.px())),
        )
        .background_color(IMG_BG)
        .corner_radius(THUMB_RADIUS.px())
        .flex(1.0),
        sized_box(
            label(name)
                .text_size(12.0)
                .text_alignment(TextAlign::Left)
                .color(TEXT),
        )
        .padding(Padding::from_vh(8.0.px(), 6.0.px())),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Stretch)
    // 关键（新版 masonry 的测量模型）：内层 flex_col 必须用**确定尺寸**。
    // 原因：`Button::layout` 用 `SizeDef::fit(size)` 按**内容尺寸**布局子项，而 Flex 在
    // measure 主轴时 `FitContent` 会被降级为 `MinContent` → 子项的 MinContent 参与计算，
    // 图片盒的 MinContent 就是图片固有宽（如 490px）→ flex_col 被撑到 490 宽，
    // 图片按 490 宽绘制、被 158 宽的卡片裁剪（只剩左侧一条细缝），
    // 同时图片盒的深色背景在行内拉出一条通栏黑带。
    // 显式给定尺寸即彻底切断与图片固有尺寸的关联：图片盒按 flex(1.0) 拿到剩余高度，
    // 交叉轴 Stretch 拿到卡片内容宽，图片于是被正确地 Contain 缩放并完整显示在卡内。
    .dims(Dimensions::new(
        Dim::Fixed((card_w - CARD_INSET).px()),
        Dim::Fixed((CARD_H - CARD_INSET).px()),
    ));
    sized_box(button(inner, move |s: &mut AppState| {
        s.open_image(original_idx)
    }))
    // 卡片宽度**显式固定**为按内容区宽度算出的值，而不是靠 flex(1.0) 均分。
    // 原因：masonry 的 Flex 在测主轴时 main_space 恒为 None，`FitContent` 会被降级为
    // `MinContent`，flex_fraction 于是按子项的 MinContent（= 图片固有宽 490px）计算，
    // 整行测量宽度 = Σ 卡片 MinContent ≫ 视口宽 → 视口里只显示得下 1~2 张卡片，
    // 且最右一张被切掉（用户实机截图：视口 1011px 却出现 546px 的卡片、行宽 2232px）。
    // 显式宽度后，整行测量宽度精确等于视口宽，卡片按列数自适应铺满。
    // 注意必须用一次 `dims()` 同时设置宽高：Style 的 width()/height() 各自会以
    // Dimensions::AUTO 为基底，后调用者会把先调用者的轴重置回 Auto。
    .dims(Dimensions::new(
        Dim::Fixed(card_w.px()),
        Dim::Fixed(CARD_H.px()),
    ))
    .background_color(CARD_BG)
    .corner_radius(14.0.px())
    .border(BORDER, CARD_BORDER.px())
    .padding(CARD_PAD.px())
    // flex 因子为 0：卡片宽度已由 Dimensions 固定，不再参与 flex 均分；
    // 这里只是为了能转换成 AnyFlexChild（xilem 的 flex 子项类型）。
    .flex(0.0)
    .into()
}

fn node_row(
    _state: &mut AppState,
    i: usize,
    flat: &[FlatNode],
) -> impl WidgetView<AppState> + use<> {
    let node = &flat[i];
    let (marker, text_color) = if node.is_folder {
        if node.expanded {
            ("▾ ", ACCENT)
        } else {
            ("▸ ", TEXT)
        }
    } else {
        ("  ", TEXT_MUTED)
    };
    let text = format!("{}{}", marker, node.name);
    // 用很小的左侧内边距表达层级，主要靠 ▸/▾ 标记区分层级。
    let mut pad = Padding::from_vh(4.0.px(), 6.0.px());
    pad.left = Length::px(6.0 + (node.depth as f64) * 10.0);
    let payload = node.payload.clone();
    // 单一 button，闭包内部按 payload 分发，避免两个不同闭包类型导致返回类型不统一。
    // 用 flex_row + .flex(1.0) 让按钮填满整行宽度，避免节点右侧出现大片空白。
    // Button 的子项默认按内容尺寸并**居中**放置，所以用 Dimensions::STRETCH 的
    // flex_row 包住 label，让文字靠左对齐（与旧版行为一致）。
    flex_row((button(
        flex_row((label(text).text_size(12.5).color(text_color),))
            .cross_axis_alignment(CrossAxisAlignment::Stretch)
            .width(Dim::Stretch)
            .height(Dim::Stretch),
        move |s: &mut AppState| match &payload {
            NodePayload::Folder(path) => s.toggle_folder(path),
            NodePayload::File(idx) => s.open_image(*idx),
        },
    )
    .background_color(NODE_BG)
    .corner_radius(6.0.px())
    .padding(pad)
    .flex(1.0),))
}

/// 后台 worker：消费主线程发来的 [`BgRequest`]，在独立 OS 线程里做重活
/// （目录扫描 / 整图解码），再用 `proxy.message` 把结果异步回传主线程。
///
/// 采用 xilem 0.4 官方推荐的 `worker` 视图 + `tokio::sync::mpsc` 异步通道
/// （而非阻塞式 `std::sync::mpsc::recv`）：`recv().await` 不会占住 tokio 工作线程，
/// 重活通过 `std::thread::spawn` 放到独立线程，结果经 Winit 事件循环回主线程触发重绘。
/// 这样大文件夹 / 大图都不会卡死 UI。
///
/// 缩略图就绪通知来自 `ThumbnailCache` 自带的 `std::sync::mpsc` 通道，无法直接喂给
/// 异步 worker，因此在 `store_sender` 里起一个极轻量的桥接线程把它转发为
/// [`BgRequest::ThumbReady`]，再由本 worker 转成 [`BgResponse::ThumbReady`] 回主线程。
fn bg_worker() -> impl View<AppState, (), ViewCtx, Element = NoElement> + use<> {
    worker(
        |proxy: MessageProxy<BgResponse>,
         mut rx: tokio::sync::mpsc::UnboundedReceiver<BgRequest>| {
            async move {
                eprintln!("DBG[worker] init_future started, waiting for requests...");
                while let Some(req) = rx.recv().await {
                    eprintln!("DBG[worker] got request: {req:?}");
                    let proxy = proxy.clone();
                    match req {
                        BgRequest::PickFolder => {
                            // 在独立线程里打开模态对话框：Windows 上 rfd 的 IFileDialog 可在
                            // 非 UI 线程使用（该线程自行初始化 COM STA）。主线程事件循环
                            // 不再被模态消息泵阻塞，消息回传保持畅通。
                            std::thread::spawn(move || {
                                let picked = FileDialog::new().pick_folder();
                                match picked {
                                    Some(dir) => {
                                        let scan = scan_directory(&dir);
                                        eprintln!(
                                            "DBG[worker] scan done ({} images), sending ScanDone",
                                            scan.image_paths.len()
                                        );
                                        if let Err(e) = proxy.message(BgResponse::ScanDone(scan)) {
                                            eprintln!(
                                                "DBG[worker] proxy.message(ScanDone) FAILED: {e:?}"
                                            );
                                        }
                                    }
                                    None => {
                                        let _ = proxy.message(BgResponse::PickCancelled);
                                    }
                                }
                            });
                        }
                        BgRequest::Scan(dir) => {
                            std::thread::spawn(move || {
                                let scan = scan_directory(&dir);
                                eprintln!(
                                    "DBG[worker] scan done ({} images), sending ScanDone",
                                    scan.image_paths.len()
                                );
                                if let Err(e) = proxy.message(BgResponse::ScanDone(scan)) {
                                    eprintln!("DBG[worker] proxy.message(ScanDone) FAILED: {e:?}");
                                }
                            });
                        }
                        BgRequest::Decode(original_idx, path) => {
                            std::thread::spawn(move || {
                                let result =
                                    decode_rgba8(&path).ok().map(|(w, h, rgba)| (rgba, w, h));
                                eprintln!(
                                    "DBG[worker] decode done idx={original_idx} ok={}",
                                    result.is_some()
                                );
                                if let Err(e) = proxy.message(BgResponse::DecodeDone(FullResult {
                                    original_idx,
                                    result,
                                })) {
                                    eprintln!(
                                        "DBG[worker] proxy.message(DecodeDone) FAILED: {e:?}"
                                    );
                                }
                            });
                        }
                        BgRequest::ThumbReady(idx) => {
                            if let Err(e) = proxy.message(BgResponse::ThumbReady(idx)) {
                                eprintln!("DBG[worker] proxy.message(ThumbReady) FAILED: {e:?}");
                            }
                        }
                    }
                }
                eprintln!("DBG[worker] request channel closed, worker exiting");
            }
        },
        |state: &mut AppState, tx: UnboundedSender<BgRequest>| {
            // 把请求发送端存进状态，供主线程（load_folder / load_full_image）使用。
            state.bg_tx = Some(tx.clone());
            eprintln!("DBG[worker] store_sender called, bg_tx set");
            // 诊断钩子：若设置了 IMG_AUTO_DIR，worker 建好后立即自动发起一次扫描。
            if let Some(dir) = state.boot_scan.take() {
                eprintln!("DBG[worker] auto-scan request: {}", dir.display());
                state.scanning = true;
                if tx.send(BgRequest::Scan(dir)).is_err() {
                    eprintln!("DBG[worker] auto-scan send FAILED (channel closed)");
                }
            }
            // 起一个桥接线程：把 ThumbnailCache 的 std 通道通知转发为 worker 请求。
            let bridge_tx = tx;
            let thumb_rx = state.thumb_rx.clone();
            std::thread::spawn(move || {
                loop {
                    let idx = {
                        let guard = thumb_rx.lock().unwrap();
                        guard.recv()
                    };
                    match idx {
                        Ok(idx) => {
                            if bridge_tx.send(BgRequest::ThumbReady(idx)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        },
        |state: &mut AppState, msg: BgResponse| match msg {
            BgResponse::ScanDone(scan) => state.on_scan_done(scan),
            BgResponse::DecodeDone(res) => state.on_full_image_ready(res),
            BgResponse::ThumbReady(idx) => state.on_thumbnail_ready(idx),
            // 用户取消了文件夹选择：退出加载态，恢复原界面。
            BgResponse::PickCancelled => state.scanning = false,
        },
    )
}

fn app_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let body = flex_col((top_bar(state), build_split(state).flex(1.0)))
        .cross_axis_alignment(CrossAxisAlignment::Stretch)
        .background_color(APP_BG);
    fork(body, (bg_worker(),))
}

// ---------------------------------------------------------------------------
// 自定义 ImageView widget（缩放 / 平移）
// ---------------------------------------------------------------------------

struct ImageViewWidget {
    image: Option<ImageBrush>,
    img_size: Option<Size>,
    zoom: f64,
    pan: Vec2,
    drag_start: Point,
    pan_start: Vec2,
    dragging: bool,
    last_click: Instant,
    last_size: Size,
    fit: ObjectFit,
    /// 诊断钩子：确保 IMG_SELFTEST_ACTION 自检只触发一次。
    selftest_done: bool,
}

impl ImageViewWidget {
    fn new(image: Option<ImageBrush>, img_size: Option<Size>, fit: ObjectFit) -> Self {
        Self {
            image,
            img_size,
            zoom: 1.0,
            pan: Vec2::ZERO,
            drag_start: Point::ORIGIN,
            pan_start: Vec2::ZERO,
            dragging: false,
            last_click: Instant::now(),
            last_size: Size::ZERO,
            fit,
            selftest_done: false,
        }
    }

    fn reset(this: &mut WidgetMut<'_, Self>) {
        this.widget.zoom = 1.0;
        this.widget.pan = Vec2::ZERO;
        this.widget.dragging = false;
        this.ctx.request_render();
    }

    fn set_image(this: &mut WidgetMut<'_, Self>, image: Option<ImageBrush>) {
        this.widget.image = image;
        this.ctx.request_render();
    }

    fn set_img_size(this: &mut WidgetMut<'_, Self>, size: Option<Size>) {
        this.widget.img_size = size;
        this.ctx.request_render();
    }

    fn zoom_by(&mut self, ctx: &mut EventCtx<'_>, factor: f64, anchor: Point) {
        let old = self.zoom;
        let new = (old * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        if (new - old).abs() < 1e-9 {
            return;
        }
        // paint 中缩放是绕原点进行的（display = translate(pan) * scale(zoom) * base），
        // 因此只需按 anchor（鼠标位置）修正 pan，光标下的图像内容即保持不动，
        // 实现真正的“以鼠标位置为中心缩放”，不会漂向角落。
        let a = anchor.to_vec2();
        let ratio = new / old;
        self.pan = a - ratio * (a - self.pan);
        self.zoom = new;
        ctx.request_render();
    }

    fn toggle_zoom(&mut self, ctx: &mut EventCtx<'_>) {
        if (self.zoom - 1.0).abs() < 1e-3 {
            let c = Point::new(self.last_size.width / 2.0, self.last_size.height / 2.0);
            self.zoom_by(ctx, 2.0, c);
        } else {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
            ctx.request_render();
        }
    }
}

impl Widget for ImageViewWidget {
    type Action = ImageViewAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        match event {
            PointerEvent::Down(pbe) => {
                // 抢文本焦点：键盘事件只发给获得焦点的 widget（焦点 fallback 是根节点，
                // 不会下钻到子节点），所以必须让 ImageView 自己拿到焦点，Esc/←/→ 才生效。
                ctx.request_focus();
                if pbe.button.is_none_or(|b| b == PointerButton::Primary) {
                    let p = ctx.local_position(pbe.state.position);
                    // 新版 ui-events 直接提供点击计数（count >= 2 即双击）；
                    // 同时保留原来的 300ms 计时作为兜底（部分平台不填 count）。
                    let now = Instant::now();
                    let double_click = pbe.state.count >= 2
                        || now.duration_since(self.last_click).as_millis() < 300;
                    if double_click {
                        self.toggle_zoom(ctx);
                    }
                    self.last_click = now;
                    self.drag_start = p;
                    self.pan_start = self.pan;
                    self.dragging = true;
                    ctx.capture_pointer();
                    ctx.set_handled();
                }
            }
            PointerEvent::Move(PointerUpdate { current, .. }) if self.dragging => {
                let p = ctx.local_position(current.position);
                self.pan = self.pan_start + (p - self.drag_start);
                ctx.request_render();
                ctx.set_handled();
            }
            PointerEvent::Move(_) => {
                // 鼠标悬停在图片上时抢焦点，无需先点击即可用 Esc/←/→ 操作。
                ctx.request_focus();
            }
            PointerEvent::Up(_) if self.dragging => {
                self.dragging = false;
                ctx.set_handled();
            }
            PointerEvent::Scroll(pse) => {
                // ui-events 的 ScrollDelta 是简单枚举，手动换算为逻辑像素。
                let dphys: PhysicalPosition<f64> = match pse.delta {
                    ScrollDelta::PixelDelta(p) => p,
                    ScrollDelta::LineDelta(x, y) => {
                        PhysicalPosition::new(x as f64 * 20.0, y as f64 * 20.0)
                    }
                    ScrollDelta::PageDelta(x, y) => {
                        PhysicalPosition::new(x as f64 * 200.0, y as f64 * 200.0)
                    }
                };
                let dpx = dphys.to_logical::<f64>(ctx.scale_factor());
                let mag = (dpx.y.abs() / 200.0).clamp(0.005, 0.06);
                // 向前滚动（dpx.y > 0）放大，向后缩小，围绕鼠标位置缩放。
                let factor = if dpx.y > 0.0 {
                    1.0 + mag
                } else {
                    1.0 / (1.0 + mag)
                };
                let anchor = ctx.local_position(pse.state.position);
                self.zoom_by(ctx, factor, anchor);
                ctx.set_handled();
            }
            PointerEvent::Gesture(ge) => {
                // 官方手势增量：双指捏合（Pinch）直接给出比例增量，无需自己算两点距离。
                if let PointerGesture::Pinch(delta) = ge.gesture {
                    let anchor = ctx.local_position(ge.state.position);
                    self.zoom_by(ctx, 1.0 + delta as f64, anchor);
                    ctx.set_handled();
                }
            }
            _ => {}
        }
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if let TextEvent::Keyboard(ke) = event {
            if ke.state.is_down() {
                match &ke.key {
                    Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                        let c = Point::new(self.last_size.width / 2.0, self.last_size.height / 2.0);
                        self.zoom_by(ctx, ZOOM_STEP, c);
                    }
                    Key::Character(s) if s.as_str() == "-" => {
                        let c = Point::new(self.last_size.width / 2.0, self.last_size.height / 2.0);
                        self.zoom_by(ctx, 1.0 / ZOOM_STEP, c);
                    }
                    Key::Character(s) if s.as_str() == "0" => {
                        self.zoom = 1.0;
                        self.pan = Vec2::ZERO;
                        ctx.request_render();
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        ctx.submit_action::<ImageViewAction>(ImageViewAction::NavigateLeft);
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        ctx.submit_action::<ImageViewAction>(ImageViewAction::NavigateRight);
                    }
                    Key::Named(NamedKey::Escape) => {
                        ctx.submit_action::<ImageViewAction>(ImageViewAction::Exit);
                    }
                    _ => {}
                }
            }
        }
    }

    fn on_access_event(
        &mut self,
        _ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        _event: &AccessEvent,
    ) {
    }

    // 必须返回 true，否则 Widget 无法获得键盘焦点，Esc/←/→ 等键盘事件永远到不了本控件
    // （masonry 的 `accepts_focus` 默认 false 且创建时缓存，request_focus 对不可聚焦控件是空操作）。
    fn accepts_focus(&self) -> bool {
        true
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    /// 新版 masonry 采用「measure（按轴查询期望长度）+ layout（给定确定尺寸）」两阶段布局，
    /// 不再有 BoxConstraints。本控件总是被父级（幻灯片区域）给定确定尺寸，
    /// 因此这里只提供合理的最小/最大偏好值。
    fn measure(
        &mut self,
        _ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        axis: Axis,
        len_req: LenReq,
        _cross_length: Option<Length>,
    ) -> Length {
        let min_size = Size::new(100.0, 80.0);
        let max_size = Size::new(640.0, 480.0);
        let min_length = min_size.get_coord(axis).px();
        let max_length = max_size.get_coord(axis).px();
        match len_req {
            LenReq::MinContent => min_length,
            LenReq::MaxContent => max_length,
            LenReq::FitContent(space) => space.clamp(min_length, max_length),
        }
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        // 记录当前尺寸，供双击复位缩放时计算中心点。
        self.last_size = size;
        // 诊断钩子：IMG_SELFTEST_ACTION=1 时，在布局阶段提交一次 Exit action，
        // 用于端到端验证自定义 widget 的 action 路由（with_action_widget）是否打通。
        // 若路由失效，日志会出现 "Got action Exit for unknown widget"。
        if !self.selftest_done && std::env::var("IMG_SELFTEST_ACTION").is_ok() {
            self.selftest_done = true;
            ctx.submit_action::<ImageViewAction>(ImageViewAction::Exit);
        }
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        // 新版 masonry 用 imaging::Painter 抽象绘制（不再直接操作 vello::Scene）。
        let content_box = ctx.content_box();
        painter.fill(content_box, APP_BG).draw();
        if let (Some(brush), Some(img_size)) = (&self.image, self.img_size) {
            let base = self.fit.affine(content_box, img_size.to_rect());
            // 绕原点缩放（scale 在 translate(pan) 之后），配合 zoom_by 的 anchor 修正，
            // 实现以光标为中心的缩放。
            let display = Affine::translate(self.pan) * Affine::scale(self.zoom) * base;
            painter.with_fill_clip(content_box, |painter| {
                painter.draw_image(brush, display);
            });
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::Image
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        _node: &mut Node,
    ) {
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }
}

struct ImageView {
    image: Option<ImageBrush>,
    img_size: Option<Size>,
    version: u64,
    fit: ObjectFit,
}

impl ViewMarker for ImageView {}

impl View<AppState, (), ViewCtx> for ImageView {
    type Element = Pod<ImageViewWidget>;
    type ViewState = ();

    fn build(&self, ctx: &mut ViewCtx, _state: &mut AppState) -> (Self::Element, Self::ViewState) {
        (
            // 关键：ImageViewWidget 会通过 ctx.submit_action 提交 ImageViewAction
            // （Esc / ← / → 导航）。必须用 with_action_widget 注册 action 路由，
            // 否则 action 会变成 “for unknown widget”，按键完全没有反应。
            // teardown 时对应调用 teardown_action_source 注销路由。
            ctx.with_action_widget(|ctx| {
                ctx.create_pod(ImageViewWidget::new(
                    self.image.clone(),
                    self.img_size,
                    self.fit,
                ))
            }),
            (),
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        _vs: &mut Self::ViewState,
        _ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        _state: &mut AppState,
    ) {
        ImageViewWidget::set_image(&mut element, self.image.clone());
        ImageViewWidget::set_img_size(&mut element, self.img_size);
        if self.version != prev.version {
            ImageViewWidget::reset(&mut element);
        }
    }

    fn teardown(
        &self,
        _vs: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        element: Mut<'_, Self::Element>,
    ) {
        // 与 build 中的 with_action_widget 配对，注销 action 路由。
        ctx.teardown_action_source(element);
    }

    fn message(
        &self,
        _vs: &mut Self::ViewState,
        message: &mut MessageCtx,
        _element: Mut<'_, Self::Element>,
        state: &mut AppState,
    ) -> MessageResult<()> {
        if let Some(action) = message.take_message::<ImageViewAction>() {
            match *action {
                ImageViewAction::NavigateLeft => state.navigate_left(),
                ImageViewAction::NavigateRight => state.navigate_right(),
                ImageViewAction::Exit => state.exit_slideshow(),
            }
            // 状态已改变，必须返回 Action 以触发重建（返回 Nop 不会重建界面）。
            return MessageResult::Action(());
        }
        MessageResult::Nop
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

fn main() {
    let mut state = AppState::new();
    // 诊断/自动化钩子：IMG_AUTO_DIR=<目录> 时启动即自动扫描该目录（无需点击）。
    if let Ok(dir) = std::env::var("IMG_AUTO_DIR") {
        if !dir.is_empty() {
            eprintln!("DBG[main] IMG_AUTO_DIR set, will auto-scan: {dir}");
            state.boot_scan = Some(std::path::PathBuf::from(dir));
        }
    }
    let event_loop = EventLoop::with_user_event();
    // 诊断钩子：IMG_WIN_W / IMG_WIN_H 指定初始窗口尺寸，便于复现不同宽度下的布局。
    let win_w: f64 = std::env::var("IMG_WIN_W")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1280.0);
    let win_h: f64 = std::env::var("IMG_WIN_H")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(768.0);
    let app = Xilem::new_simple(
        state,
        app_view,
        WindowOptions::new("Image_Loader").with_initial_inner_size(LogicalSize::new(win_w, win_h)),
    );
    if let Err(e) = app.run_in(event_loop) {
        eprintln!("应用退出：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// masonry_testing 的 TestHarness 使用全局 tracing backend，多个 harness 并行运行会
    /// 相互争抢而 panic（masonry_core/src/app/tracing_backend.rs）。用这把锁把它们串行化。
    static HARNESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn harness_guard() -> std::sync::MutexGuard<'static, ()> {
        HARNESS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn thumb_widget_clips_to_rounded_corners() {
        let _guard = harness_guard();
        use xilem::masonry::core::{NewWidget, WidgetTag};
        use xilem::masonry::peniko::{ImageAlphaType, ImageBrush, ImageData, ImageFormat};
        use xilem::masonry::testing::TestHarness;
        use xilem::masonry::theme::default_property_set;

        // 一张 100x100 的纯不透明蓝色图。
        let mut data = vec![0u8; 100 * 100 * 4];
        for px in data.chunks_exact_mut(4) {
            px[0] = 30;
            px[1] = 90;
            px[2] = 220;
            px[3] = 255;
        }
        let brush = ImageBrush::new(ImageData {
            data: data.into(),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 100,
            height: 100,
        });

        let tag = WidgetTag::unique();
        let widget = NewWidget::new(ThumbWidget::new(
            brush,
            Size::new(100.0, 100.0),
            THUMB_RADIUS,
        ))
        .with_tag(tag);
        let mut harness = TestHarness::create_with_size(default_property_set(), widget, (100, 100));
        let img = harness.render();

        // 中心应是实心蓝色。
        let center = img.get_pixel(50, 50);
        assert_eq!(
            (center[0], center[1], center[2], center[3]),
            (30, 90, 220, 255),
            "缩略图中心应是实心图片"
        );

        // 四个角必须**不是图片色**（被圆角裁掉了，露出背景）。
        // 注意：测试画布背景是不透明的深灰，不是透明，所以不能断言 alpha == 0。
        for (x, y) in [(0, 0), (99, 0), (0, 99), (99, 99)] {
            let p = img.get_pixel(x, y);
            assert_ne!(
                (p[0], p[1], p[2]),
                (30, 90, 220),
                "圆角 ({x},{y}) 应被裁掉（不应显示图片本色），实际 rgb={:?}",
                (p[0], p[1], p[2])
            );
        }

        // 而圆角内侧（离角 5px 处）必须是实心图片色。
        for (x, y) in [(5, 5), (94, 5), (5, 94), (94, 94)] {
            let p = img.get_pixel(x, y);
            assert_eq!(
                (p[0], p[1], p[2], p[3]),
                (30, 90, 220, 255),
                "圆角内侧 ({x},{y}) 应是实心图片，实际 rgba={:?}",
                (p[0], p[1], p[2], p[3])
            );
        }
    }

    /// 自适应列数必须随宽度单调不减，且始终落在 [MIN_COLS, MAX_COLS] 内。
    #[test]
    fn grid_cols_adapts_to_width() {
        let widths = [200.0, 400.0, 700.0, 1060.0, 1500.0, 2560.0];
        let mut prev = 0usize;
        for w in widths {
            let c = grid_cols(w);
            assert!(
                (MIN_COLS..=MAX_COLS).contains(&c),
                "宽度 {w} 下列数 {c} 越界"
            );
            assert!(c >= prev, "宽度 {w} 列数 {c} 反而变少了（上一档 {prev}）");
            prev = c;
        }
        // 未测量时（pane_width = 0）使用保守默认值，绝不能是 0 列。
        assert!(grid_cols(0.0) >= MIN_COLS);
    }

    /// 回归测试：新版 masonry 的 flex 在**交叉轴**上默认按子项固有尺寸布局（FitContent），
    /// 会让卡片里的图片按原始像素宽（例如 512px）溢出到卡片外面、盖住卡片边框；
    /// 显式 `CrossAxisAlignment::Stretch` 才能让图片盒填满卡片内容宽。
    ///
    /// 这正是 `card_view` 里必须加 `.cross_axis_alignment(CrossAxisAlignment::Stretch)`
    /// 的原因（xilem 的 flex 视图默认是 Center）。
    #[test]
    fn flex_cross_axis_center_overflows_vs_stretch_fills() {
        let _guard = harness_guard();
        use xilem::masonry::core::{NewWidget, WidgetTag};
        use xilem::masonry::layout::AsUnit;
        use xilem::masonry::testing::TestHarness;
        use xilem::masonry::theme::default_property_set;
        use xilem::masonry::widgets::{Flex, SizedBox};

        /// 构造与卡片内部相同的结构：竖向 flex，子项是一个「固有宽度 512px」的盒子。
        fn child_width(align: CrossAxisAlignment) -> f64 {
            let tag = WidgetTag::unique();
            let img = NewWidget::new(SizedBox::empty().width(512.0.px()).height(100.0.px()))
                .with_tag(tag);
            let flex = Flex::column()
                .cross_axis_alignment(align)
                .with(img, 1.0)
                .prepare();
            let harness = TestHarness::create_with_size(default_property_set(), flex, (200, 300));
            harness.get_widget(tag).ctx().border_box().size().width
        }

        // Center（xilem flex 视图的默认值）：按子项固有尺寸 → 512px > 容器 200px，溢出卡片
        let centered = child_width(CrossAxisAlignment::Center);
        assert!(
            centered > 200.0,
            "Center 下子项应按固有尺寸溢出，实际 {centered}"
        );

        // Stretch：填满容器宽度 → 200px，不再溢出
        let stretched = child_width(CrossAxisAlignment::Stretch);
        assert!(
            (stretched - 200.0).abs() < 0.5,
            "Stretch 下子项宽度应等于容器宽 200px，实际 {stretched}"
        );
    }

    /// 回归测试：新版 masonry 的 `Button::layout` 用 `SizeDef::fit(size)`——按**内容尺寸**
    /// 而非填满按钮来布局子项；且 Flex 的 measure 阶段会忽略 `cross_axis_alignment`。
    /// 两者叠加，按钮内的竖向 flex 会被图片的固有尺寸（如 490px）撑开，
    /// 导致缩略图溢出卡片、盖在卡片边框上。
    ///
    /// 修复手段：给按钮内的 flex 显式 `Dimensions::STRETCH`，让它填满按钮空间。
    #[test]
    fn button_child_with_stretch_stays_inside() {
        let _guard = harness_guard();
        use xilem::masonry::core::{NewWidget, PropertySet, WidgetTag};
        use xilem::masonry::peniko::{ImageAlphaType, ImageBrush, ImageData, ImageFormat};
        use xilem::masonry::properties::Dimensions;
        use xilem::masonry::testing::TestHarness;
        use xilem::masonry::theme::default_property_set;
        use xilem::masonry::widgets::{Button, Flex, Image, Label};

        /// 在 Button 里放一个「图片 + 标签」的竖向 flex，返回图片盒的实际尺寸。
        fn image_box_size(stretch: bool) -> (f64, f64) {
            let tag = WidgetTag::unique();
            let img = NewWidget::new(Image::new(ImageBrush::new(ImageData {
                data: vec![128u8; 490 * 490 * 4].into(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: 490,
                height: 490,
            })))
            .with_tag(tag);
            let inner = Flex::column()
                .cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with(img, 1.0)
                .with_fixed(NewWidget::new(Label::new("img_00001.png")))
                .prepare();
            let inner = if stretch {
                inner.with_props(PropertySet::one(Dimensions::STRETCH))
            } else {
                inner
            };
            let btn = Button::new(inner).prepare();
            let h = TestHarness::create_with_size(default_property_set(), btn, (200, 300));
            let s = h.get_widget(tag).ctx().border_box().size();
            (s.width, s.height)
        }

        // 不加 STRETCH：按钮按内容尺寸布局，图片被撑到固有尺寸 490px → 溢出卡片
        let (w_without, _) = image_box_size(false);
        assert!(
            w_without > 200.0,
            "未加 STRETCH 时图片应溢出按钮（实际 {w_without}）"
        );

        // 加 STRETCH：flex 填满按钮空间，图片被限制在按钮内
        let (w_with, _) = image_box_size(true);
        assert!(
            w_with <= 200.0,
            "加 STRETCH 后图片宽度应不超过按钮宽度 200px，实际 {w_with}"
        );
    }

    /// 回归测试：masonry 的 `VirtualScroll` 用 `SizeDef::fit`（即 FitContent）测量每个行 widget，
    /// 而 Flex 的 FitContent 实现是 `space.max(MinContent)`，且测主轴时 main_space 恒为 None，
    /// 于是 flex_fraction 按子项的 **MinContent**（= 图片固有宽 490px）计算 →
    /// 整行测量宽度 = Σ 卡片 MinContent ≫ 视口宽 → 视口里只显示得下 1~2 张卡片，
    /// 最右一张被切掉（用户实机截图：视口 1011px 却出现 546px 卡片、行宽 2232px）。
    ///
    /// 修复手段：给卡片**显式固定宽度**（由内容区宽度算出），使整行测量宽度精确等于视口宽。
    #[test]
    fn virtualscroll_fitcontent_row_width_stays_in_viewport() {
        let _guard = harness_guard();
        use xilem::masonry::core::{
            ChildrenIds, LayoutCtx, MeasureCtx, NewWidget, PaintCtx, PropertiesRef, PropertySet,
            RegisterCtx, Widget, WidgetPod,
        };
        use xilem::masonry::kurbo::{Axis, Point, Size};
        use xilem::masonry::layout::{AsUnit, Dim, LayoutSize, LenDef, LenReq, Length, SizeDef};
        use xilem::masonry::peniko::{ImageAlphaType, ImageBrush, ImageData, ImageFormat};
        use xilem::masonry::properties::{BorderWidth, Dimensions, Gap, Padding};
        use xilem::masonry::testing::TestHarness;
        use xilem::masonry::theme::default_property_set;
        use xilem::masonry::widgets::{Button, Flex, Image, Label, SizedBox};

        /// 模拟 VirtualScroll 的容器：按 SizeDef::fit 测量并布局子项。
        struct FitContainer {
            child: WidgetPod<dyn Widget>,
        }
        impl Widget for FitContainer {
            type Action = xilem::masonry::core::NoAction;
            fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
                ctx.register_child(&mut self.child);
            }
            fn measure(
                &mut self,
                ctx: &mut MeasureCtx<'_>,
                _props: &PropertiesRef<'_>,
                axis: Axis,
                len_req: LenReq,
                _cross_length: Option<Length>,
            ) -> Length {
                ctx.compute_length(
                    &mut self.child,
                    len_req.into(),
                    LayoutSize::NONE,
                    axis,
                    None,
                )
            }
            fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
                let auto_size = SizeDef::fit(size).with(Axis::Vertical, LenDef::MaxContent);
                let child_size = ctx.compute_size(&mut self.child, auto_size, size.into());
                ctx.run_layout(&mut self.child, child_size);
                ctx.place_child(&mut self.child, Point::ORIGIN);
            }
            fn paint(
                &mut self,
                _ctx: &mut PaintCtx<'_>,
                _props: &PropertiesRef<'_>,
                _painter: &mut xilem::masonry::imaging::Painter<'_>,
            ) {
            }
            fn accessibility_role(&self) -> xilem::masonry::accesskit::Role {
                xilem::masonry::accesskit::Role::GenericContainer
            }
            fn accessibility(
                &mut self,
                _ctx: &mut xilem::masonry::core::AccessCtx<'_>,
                _props: &PropertiesRef<'_>,
                _node: &mut xilem::masonry::accesskit::Node,
            ) {
            }
            fn children_ids(&self) -> ChildrenIds {
                ChildrenIds::from_slice(&[self.child.id()])
            }
        }

        const COLS: usize = 4;
        const VIEW_W: f64 = 1011.0;

        fn build_row(
            stretch_image: bool,
            row_tag: xilem::masonry::core::WidgetTag<Flex>,
        ) -> NewWidget<FitContainer> {
            build_row2(stretch_image, row_tag, None)
        }

        fn build_row2(
            stretch_image: bool,
            row_tag: xilem::masonry::core::WidgetTag<Flex>,
            fixed_card_w: Option<f64>,
        ) -> NewWidget<FitContainer> {
            let mut row = Flex::row().cross_axis_alignment(CrossAxisAlignment::Center);
            for _ in 0..COLS {
                let mut img = NewWidget::new(Image::new(ImageBrush::new(ImageData {
                    data: vec![128u8; 490 * 490 * 4].into(),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: 490,
                    height: 490,
                })));
                if stretch_image {
                    img =
                        img.with_props(PropertySet::one(Dimensions::AUTO.with_width(Dim::Stretch)));
                }
                let inner = Flex::column()
                    .cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with(img, 1.0)
                    .with_fixed(NewWidget::new(Label::new("img_00001.png")))
                    .prepare()
                    .with_props(PropertySet::one(Dimensions::STRETCH));
                let dims = match fixed_card_w {
                    Some(w) => Dimensions::new(Dim::Fixed(w.px()), Dim::Fixed(200.0.px())),
                    None => Dimensions::AUTO.with_height(Dim::Fixed(200.0.px())),
                };
                let card = NewWidget::new(SizedBox::new(Button::new(inner).prepare())).with_props(
                    PropertySet::from((
                        dims,
                        Padding::from_vh(10.0.px(), 10.0.px()),
                        BorderWidth::all(1.0.px()),
                    )),
                );
                row = row.with(card, 1.0);
            }
            let row = row
                .prepare()
                .with_tag(row_tag)
                .with_props(PropertySet::one(Gap::new(16.0.px())));
            let child: WidgetPod<dyn Widget> = row.erased().to_pod();
            NewWidget::new(FitContainer { child })
        }

        // 不给固定宽度（复现 bug）：行被卡片 MinContent 撑到远宽于视口
        {
            let row_tag = xilem::masonry::core::WidgetTag::unique();
            let root = build_row2(false, row_tag, None);
            let harness =
                TestHarness::create_with_size(default_property_set(), root, (VIEW_W as u32, 700));
            let w = harness.get_widget(row_tag);
            let ctx = w.ctx();
            let r = ctx.window_transform().transform_rect_bbox(ctx.border_box());
            assert!(
                r.width() > VIEW_W * 1.5,
                "未固定宽度时行应被撑爆（实际 {:.1}）",
                r.width()
            );
        }

        // 给固定宽度（修复方案）：整行宽度精确等于视口宽，卡片均分且不溢出
        {
            let row_tag = xilem::masonry::core::WidgetTag::unique();
            let root = build_row2(false, row_tag, Some(240.0));
            let harness =
                TestHarness::create_with_size(default_property_set(), root, (VIEW_W as u32, 700));
            let w = harness.get_widget(row_tag);
            let ctx = w.ctx();
            let r = ctx.window_transform().transform_rect_bbox(ctx.border_box());
            assert!(
                (r.width() - VIEW_W).abs() < 1.0,
                "固定宽度时行宽应等于视口宽 {VIEW_W}，实际 {:.1}",
                r.width()
            );
        }
    }

    /// 渲染真实的网格布局为 PNG，用于肉眼核对卡片是否被限制在卡片框内、
    /// 整行是否精确铺满视口宽度。
    #[test]
    fn render_grid_snapshot() {
        let _guard = harness_guard();
        use xilem::masonry::core::{
            ChildrenIds, LayoutCtx, MeasureCtx, NewWidget, PaintCtx, PropertiesRef, PropertySet,
            RegisterCtx, Widget, WidgetPod,
        };
        use xilem::masonry::kurbo::{Axis, Point, Size};
        use xilem::masonry::layout::{AsUnit, Dim, LayoutSize, LenDef, LenReq, Length, SizeDef};
        use xilem::masonry::peniko::{Color, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
        use xilem::masonry::properties::{
            Background, BorderWidth, CornerRadius, Dimensions, Gap, ObjectFit, Padding,
        };
        use xilem::masonry::testing::TestHarness;
        use xilem::masonry::theme::default_property_set;
        use xilem::masonry::widgets::{Button, Flex, Image, Label, SizedBox};

        /// 模拟 VirtualScroll：按 SizeDef::fit 测量并布局每个行 widget。
        struct FitContainer {
            child: WidgetPod<dyn Widget>,
        }
        impl Widget for FitContainer {
            type Action = xilem::masonry::core::NoAction;
            fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
                ctx.register_child(&mut self.child);
            }
            fn measure(
                &mut self,
                ctx: &mut MeasureCtx<'_>,
                _props: &PropertiesRef<'_>,
                axis: Axis,
                len_req: LenReq,
                _cross_length: Option<Length>,
            ) -> Length {
                ctx.compute_length(
                    &mut self.child,
                    len_req.into(),
                    LayoutSize::NONE,
                    axis,
                    None,
                )
            }
            fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
                let auto_size = SizeDef::fit(size).with(Axis::Vertical, LenDef::MaxContent);
                let child_size = ctx.compute_size(&mut self.child, auto_size, size.into());
                ctx.run_layout(&mut self.child, child_size);
                ctx.place_child(&mut self.child, Point::ORIGIN);
            }
            fn paint(
                &mut self,
                _ctx: &mut PaintCtx<'_>,
                _props: &PropertiesRef<'_>,
                _painter: &mut xilem::masonry::imaging::Painter<'_>,
            ) {
            }
            fn accessibility_role(&self) -> xilem::masonry::accesskit::Role {
                xilem::masonry::accesskit::Role::GenericContainer
            }
            fn accessibility(
                &mut self,
                _ctx: &mut xilem::masonry::core::AccessCtx<'_>,
                _props: &PropertiesRef<'_>,
                _node: &mut xilem::masonry::accesskit::Node,
            ) {
            }
            fn children_ids(&self) -> ChildrenIds {
                ChildrenIds::from_slice(&[self.child.id()])
            }
        }

        const COLS: usize = 5;
        const ROWS: usize = 3;
        const VIEW_W: f64 = 1011.0;
        const GAP: f64 = 16.0;
        const PAD_X: f64 = 14.0;
        const GUTTER: f64 = 16.0;
        const CARD_H: f64 = 200.0;

        // 与 AppState::grid_cols / card_width 完全相同的算法
        let inner = VIEW_W - 2.0 * PAD_X - GUTTER;
        let cols = ((inner + GAP) / (180.0 + GAP)).floor() as usize;
        let card_w = (inner - (cols - 1) as f64 * GAP) / cols as f64;
        println!("[render] 列数 {cols}，每卡 {card_w:.1}px，内边距后可用 {inner:.1}px");

        fn make_image(seed: u32) -> ImageBrush {
            let mut data = vec![0u8; 490 * 490 * 4];
            for y in 0..490 {
                for x in 0..490 {
                    let i = (y * 490 + x) * 4;
                    data[i] = ((x * 255 / 490) as u32 + seed * 37 % 200) as u8;
                    data[i + 1] = (y * 255 / 490) as u8;
                    data[i + 2] = (seed * 61 % 255) as u8;
                    data[i + 3] = 255;
                }
            }
            ImageBrush::new(ImageData {
                data: data.into(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: 490,
                height: 490,
            })
        }

        let mut img_tags: Vec<xilem::masonry::core::WidgetTag<xilem::masonry::widgets::Image>> =
            Vec::new();
        let mut outer = Flex::column();
        for r in 0..ROWS {
            let mut row = Flex::row().cross_axis_alignment(CrossAxisAlignment::Center);
            for c in 0..COLS {
                let idx = r * COLS + c;
                // 与 card_view 完全一致的结构：图片包 sized_box、标签包 sized_box+padding
                let img_tag = xilem::masonry::core::WidgetTag::unique();
                img_tags.push(img_tag);
                let img = NewWidget::new(Image::new(make_image(idx as u32)))
                    .with_tag(img_tag)
                    .with_props(PropertySet::one(ObjectFit::Contain));
                let img_box = NewWidget::new(SizedBox::new(img)).with_props(PropertySet::from((
                    Background::Color(Color::from_rgb8(14, 16, 21)),
                    CornerRadius::all(8.0.px()),
                )));
                let label_box = NewWidget::new(SizedBox::new(NewWidget::new(Label::new(format!(
                    "img_{idx:05}.png"
                )))))
                .with_props(PropertySet::one(Padding::from_vh(8.0.px(), 6.0.px())));
                // 内层 flex_col 用**确定尺寸**，切断与图片固有尺寸（490px）的关联：
                // 否则图片盒的 MinContent = 490 → flex_col 测量宽度被撑到 490 →
                // 图片按 490 宽绘制，被 158 宽的卡片裁剪（只剩左侧一条细缝），
                // 且图片盒的深色背景在行内拉出通栏黑带。
                let inner_col = Flex::column()
                    .cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with(img_box, 1.0)
                    .with_fixed(label_box)
                    .prepare()
                    .with_props(PropertySet::one(Dimensions::new(
                        Dim::Fixed((card_w - 22.0).px()),
                        Dim::Fixed((CARD_H - 22.0).px()),
                    )));
                let card = NewWidget::new(SizedBox::new(Button::new(inner_col).prepare()))
                    .with_props(PropertySet::from((
                        Dimensions::new(Dim::Fixed(card_w.px()), Dim::Fixed(CARD_H.px())),
                        Padding::from_vh(10.0.px(), 10.0.px()),
                        BorderWidth::all(1.0.px()),
                        Background::Color(Color::from_rgb8(34, 38, 48)),
                    )));
                row = row.with(card, 0.0);
            }
            let row = row.prepare().with_props(PropertySet::from((
                Gap::new(GAP.px()),
                Padding::from_vh(8.0.px(), PAD_X.px()),
            )));
            let child: WidgetPod<dyn Widget> = row.erased().to_pod();
            outer = outer.with(NewWidget::new(FitContainer { child }), 0.0);
        }
        let root = outer.prepare();
        let mut harness =
            TestHarness::create_with_size(default_property_set(), root, (VIEW_W as u32, 700));
        // 断言：每张图片都必须完整落在卡片内容区内（不能溢出、不能只剩一条细缝）。
        for tag in &img_tags {
            let w = harness.get_widget(*tag);
            let ctx = w.ctx();
            let r = ctx.window_transform().transform_rect_bbox(ctx.border_box());
            assert!(
                r.width() <= card_w - 22.0 + 1.0,
                "图片盒宽度 {} 超出卡片内容宽 {:.1}",
                r.width(),
                card_w - 22.0
            );
            assert!(
                r.height() <= CARD_H - 22.0 + 1.0,
                "图片盒高度 {} 超出卡片内容高 {:.1}",
                r.height(),
                CARD_H - 22.0
            );
        }
        let img = harness.render();
        let out = std::path::Path::new("_grid_render.png");
        img.save(out).unwrap();
        println!(
            "[render] 已保存 {}，全部 {} 张图片均在卡内",
            out.display(),
            img_tags.len()
        );
    }

    /// 关键不变式：按 grid_cols 算出的列数下，每张卡片的实际宽度必须为正
    /// （否则最右列会被挤出视口 = “右侧显示不完整”），且已经无法再塞下一列
    /// （否则说明列数算少了、浪费空间）。
    #[test]
    fn grid_cols_cards_fit_without_truncation() {
        for w in [300.0, 500.0, 800.0, 1060.0, 1400.0, 1920.0, 2560.0] {
            let cols = grid_cols(w);
            let inner = w - 2.0 * ROW_PAD_X - SCROLLBAR_GUTTER;
            let card_w = (inner - (cols - 1) as f64 * GAP) / cols as f64;
            assert!(
                card_w > 0.0,
                "宽度 {w} 下第 {cols} 列宽度为 {card_w:.1}px，会被截断"
            );
            let next_w = (inner - cols as f64 * GAP) / (cols + 1) as f64;
            assert!(
                next_w < TARGET_CARD_W,
                "宽度 {w} 下列数 {cols} 偏少：还能再放一列 {next_w:.1}px"
            );
        }
    }
}
