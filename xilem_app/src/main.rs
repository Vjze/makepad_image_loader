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
use image_loader_core::scanner::{scan_directory, FileTreeNode, ScanResult};
use image_loader_core::thumbnail::{THUMB_MAX_BITMAPS, THUMB_MAX_IN_FLIGHT};

use ui_events::keyboard::{Key, NamedKey};

use xilem::core::{
    fork, MessageContext, MessageProxy, MessageResult, Mut, NoElement, View, ViewMarker,
};
use xilem::core::one_of::OneOf2;
use xilem::masonry::accesskit::{Node, Role};
use xilem::masonry::core::{
    AccessCtx, AccessEvent, Axis, BoxConstraints, ChildrenIds, EventCtx, LayoutCtx, PaintCtx,
    PointerButton, PointerEvent, PointerUpdate, PropertiesMut, PropertiesRef, RegisterCtx,
    ScrollDelta, TextEvent, Widget, WidgetMut,
};
use xilem::masonry::dpi::{LogicalSize, PhysicalPosition};
use xilem::masonry::kurbo::{Affine, Point, Size, Vec2};
use xilem::masonry::peniko::{
    BlendMode, Color, ImageAlphaType, ImageBrush, ImageData, ImageFormat,
};
use xilem::masonry::properties::types::Length;
use xilem::masonry::properties::Padding;
use xilem::masonry::util::fill_color;
use xilem::masonry::vello::Scene;
use xilem::style::Style as _;
use xilem::view::{
    button, flex_col, flex_row, image, indexed_stack, label, sized_box, split, virtual_scroll,
    worker, AnyFlexChild, CrossAxisAlignment, FlexExt, FlexSpacer, ObjectFit,
};
use xilem::{EventLoop, FontWeight, Pod, TextAlign, ViewCtx, WidgetView, WindowOptions, Xilem};

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
const ACCENT_ACTIVE: Color = Color::from_rgb8(99, 146, 255);
const TEXT: Color = Color::from_rgb8(228, 231, 237);
const TEXT_MUTED: Color = Color::from_rgb8(150, 157, 170);
const BORDER: Color = Color::from_rgb8(46, 51, 62);

use rfd::FileDialog;

// 网格每行卡片数（Makepad 版根据宽度动态算，这里固定列数做等价功能）。
const COLS: usize = 5;
// 卡片尺寸。
const CARD_W: f64 = 220.0;
const CARD_H: f64 = 250.0;

// 侧栏目标宽度（与 Makepad 版 split 设 200px 对应；split_point 为比例值）。
const SIDEBAR_PX: f64 = 220.0;
const WINDOW_W: f64 = 1280.0;

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
            boot_scan: None,
        }
    }

    fn num_images(&self) -> usize {
        self.filtered_image_idxs.len()
    }

    fn num_rows(&self) -> usize {
        self.num_images().div_ceil(COLS)
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
    }

    fn open_slideshow(&mut self) {
        self.in_slideshow = true;
        if self.current_image_idx.is_none() && !self.filtered_image_idxs.is_empty() {
            self.set_current_image(Some(0));
        }
    }

    fn exit_slideshow(&mut self) {
        self.in_slideshow = false;
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
    flex_row((
        button(
            label("打开文件夹")
                .text_size(13.0)
                .weight(FontWeight::MEDIUM)
                .color(TEXT),
            |s: &mut AppState| s.load_folder(),
        )
        .background_color(ACCENT)
        .active_background_color(ACCENT_ACTIVE)
        .corner_radius(8.0)
        .padding(Padding::from_vh(8.0, 14.0)),
        label(format!("共 {} 张", state.image_paths.len()))
            .text_size(13.0)
            .text_alignment(TextAlign::Left)
            .color(TEXT_MUTED),
        label("").flex(1.0),
        button(
            label("幻灯片模式")
                .text_size(13.0)
                .weight(FontWeight::MEDIUM)
                .color(TEXT),
            |s: &mut AppState| s.open_slideshow(),
        )
        .background_color(ARROW_BG)
        .active_background_color(STATUS_BG)
        .corner_radius(8.0)
        .padding(Padding::from_vh(8.0, 14.0)),
        button(
            label("退出").text_size(13.0).weight(FontWeight::MEDIUM).color(TEXT),
            |s: &mut AppState| s.exit_slideshow(),
        )
        .background_color(STATUS_BG)
        .active_background_color(ARROW_BG)
        .corner_radius(8.0)
        .padding(Padding::from_vh(8.0, 14.0)),
    ))
    .gap(Length::px(10.0))
    .cross_axis_alignment(CrossAxisAlignment::Center)
    .background_color(TOPBAR_BG)
    .padding(Padding::from_vh(10.0, 14.0))
}

fn sidebar_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 诊断开关：NO_TREE=1 时文件树为空（隔离 virtual_scroll 死循环来源）。
    let no_tree = std::env::var("NO_TREE").is_ok();
    let tree = if no_tree {
        OneOf2::A(virtual_scroll(0..0, |_: &mut AppState, _i: i64| flex_row(())))
    } else {
        OneOf2::B(file_tree_view(state))
    };
    flex_col((label("文件列表").text_size(11.0).color(TEXT_MUTED), tree.flex(1.0)))
        .gap(Length::px(6.0))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
        .background_color(SIDEBAR_BG)
        .padding(Padding::from_vh(10.0, 6.0))
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
    // split_point 是 [0,1] 比例值；用目标窗口宽度换算侧栏像素宽度。
    split(sidebar, content)
        .split_axis(Axis::Horizontal)
        .split_point(if state.file_tree_open && !no_tree {
            SIDEBAR_PX / WINDOW_W
        } else {
            0.0
        })
        .draggable(true)
        .bar_size(Length::px(4.0))
        .min_bar_area(Length::px(4.0))
}

fn content_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let in_slideshow = state.in_slideshow;
    indexed_stack((image_browser(state), slideshow(state)))
        .active(if in_slideshow { 1 } else { 0 })
        .background_color(APP_BG)
}

fn image_browser(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    // 扫描后台进行时显示加载提示；扫描完成后切换到缩略图网格。
    // 两条分支共用同一个 indexed_stack 类型，仅 active 不同，因此返回值类型统一。
    let loading = flex_col((
        label("正在加载文件夹…").text_size(15.0).color(TEXT_MUTED),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Center)
    .background_color(APP_BG);
    let nrows = state.num_rows().max(1) as i64;
    let grid = virtual_scroll(0..nrows, |s: &mut AppState, row_idx: i64| {
        row_view(s, row_idx as usize)
    });
    indexed_stack((loading, grid)).active(if state.scanning { 0 } else { 1 })
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
        sized_box(arrow_button("‹", |s: &mut AppState| s.navigate_left()))
            .width(Length::px(56.0)),
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
                .active_background_color(ACCENT_ACTIVE)
                .corner_radius(8.0)
                .border(BORDER, 1.0)
                .padding(Padding::from_vh(7.0, 16.0)),
            ))
            .gap(Length::px(12.0))
            .cross_axis_alignment(CrossAxisAlignment::Center)
            .background_color(STATUS_BG)
            .padding(Padding::from_vh(8.0, 12.0)),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
        .flex(1.0),
        sized_box(arrow_button("›", |s: &mut AppState| s.navigate_right()))
            .width(Length::px(56.0)),
    ))
    .background_color(APP_BG)
}

fn arrow_button(
    symbol: &'static str,
    action: fn(&mut AppState),
) -> impl WidgetView<AppState> + use<> {
    button(label(symbol).text_size(22.0).color(TEXT), action)
        .background_color(ARROW_BG)
        .active_background_color(STATUS_BG)
        .corner_radius(10.0)
        .padding(Padding::from_vh(2.0, 10.0))
}

fn row_view(state: &mut AppState, row_idx: usize) -> impl WidgetView<AppState> + use<> {
    let first = row_idx * COLS;
    let total = state.filtered_image_idxs.len();
    let count = COLS.min(total.saturating_sub(first));
    // `flex_row` 接受固定长度数组（ViewSequence 仅对数组/元组实现，不对 Vec 实现），
    // 因此用长度为 COLS 的数组，未使用的槽位用 0 尺寸占位 spacer 填充。
    let mut children: [AnyFlexChild<AppState>; COLS] =
        std::array::from_fn(|_| FlexSpacer::Fixed(Length::px(0.0)).into());
    for k in 0..count {
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
        children[k] = card_view(brush, name, original_idx);
    }
    flex_row(children)
        .gap(Length::px(16.0))
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .padding(Padding::from_vh(8.0, 14.0))
}

fn card_view(brush: ImageBrush, name: String, original_idx: usize) -> AnyFlexChild<AppState> {
    let inner = flex_col((
        sized_box(image(brush).fit(ObjectFit::Contain))
            .background_color(IMG_BG)
            .corner_radius(8.0)
            .flex(1.0),
        sized_box(
            label(name)
                .text_size(12.0)
                .text_alignment(TextAlign::Left)
                .color(TEXT),
        )
        .padding(Padding::from_vh(8.0, 6.0)),
    ));
    sized_box(button(inner, move |s: &mut AppState| s.open_image(original_idx)))
        .width(Length::px(CARD_W))
        .height(Length::px(CARD_H))
        .background_color(CARD_BG)
        .corner_radius(14.0)
        .border(BORDER, 1.0)
        .padding(10.0)
        .into_any_flex()
}

fn file_tree_view(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let flat = state.flat_tree();
    let len = flat.len() as i64;
    virtual_scroll(0..len, move |s: &mut AppState, i: i64| {
        node_row(s, i as usize, &flat)
    })
}

fn node_row(_state: &mut AppState, i: usize, flat: &[FlatNode]) -> impl WidgetView<AppState> + use<> {
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
    let mut pad = Padding::from_vh(4.0, 6.0);
    pad.left += (node.depth as f64) * 10.0;
    let payload = node.payload.clone();
    // 单一 button，闭包内部按 payload 分发，避免两个不同闭包类型导致返回类型不统一。
    // 用 flex_row + .flex(1.0) 让按钮填满整行宽度，避免节点右侧出现大片空白。
    flex_row((button(
        label(text).text_size(12.5).color(text_color),
        move |s: &mut AppState| match &payload {
            NodePayload::Folder(path) => s.toggle_folder(path),
            NodePayload::File(idx) => s.open_image(*idx),
        },
    )
    .background_color(NODE_BG)
    .active_background_color(STATUS_BG)
    .corner_radius(6.0)
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
        |proxy: MessageProxy<BgResponse>, mut rx: tokio::sync::mpsc::UnboundedReceiver<BgRequest>| {
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
                                let result = decode_rgba8(&path)
                                    .ok()
                                    .map(|(w, h, rgba)| (rgba, w, h));
                                eprintln!(
                                    "DBG[worker] decode done idx={original_idx} ok={}",
                                    result.is_some()
                                );
                                if let Err(e) =
                                    proxy.message(BgResponse::DecodeDone(FullResult {
                                        original_idx,
                                        result,
                                    }))
                                {
                                    eprintln!("DBG[worker] proxy.message(DecodeDone) FAILED: {e:?}");
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
            std::thread::spawn(move || loop {
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
        .cross_axis_alignment(CrossAxisAlignment::Fill)
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
}

impl ImageViewWidget {
    fn new(
        image: Option<ImageBrush>,
        img_size: Option<Size>,
        fit: ObjectFit,
    ) -> Self {
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
                    let now = Instant::now();
                    if now.duration_since(self.last_click).as_millis() < 300 {
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
                // ui-events 0.2.0 的 ScrollDelta 是简单枚举，手动换算为逻辑像素。
                let dphys: PhysicalPosition<f64> = match pse.delta {
                    ScrollDelta::PixelDelta(p) => p,
                    ScrollDelta::LineDelta(x, y) => {
                        PhysicalPosition::new(x as f64 * 20.0, y as f64 * 20.0)
                    }
                    ScrollDelta::PageDelta(x, y) => {
                        PhysicalPosition::new(x as f64 * 200.0, y as f64 * 200.0)
                    }
                };
                let dpx = dphys.to_logical::<f64>(ctx.get_scale_factor());
                let mag = (dpx.y.abs() / 200.0).clamp(0.005, 0.06);
                // 向前滚动（dpx.y > 0）放大，向后缩小，围绕鼠标位置缩放。
                let factor = if dpx.y > 0.0 { 1.0 + mag } else { 1.0 / (1.0 + mag) };
                let anchor = ctx.local_position(pse.state.position);
                self.zoom_by(ctx, factor, anchor);
                ctx.set_handled();
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

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        bc: &BoxConstraints,
    ) -> Size {
        let mut size = bc.max();
        if !size.is_finite() {
            size = Size::new(640.0, 480.0);
        }
        self.last_size = size;
        size
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        scene: &mut Scene,
    ) {
        let size = ctx.size();
        let rect = size.to_rect();
        fill_color(scene, &rect, APP_BG);
        if let (Some(brush), Some(img_size)) = (&self.image, self.img_size) {
            let base = self.fit.affine_to_fill(size, img_size);
            // 绕原点缩放（scale 在 translate(pan) 之后），配合 zoom_by 的 anchor 修正，
            // 实现以光标为中心的缩放。
            let display = Affine::translate(self.pan) * Affine::scale(self.zoom) * base;
            scene.push_layer(BlendMode::default(), 1., Affine::IDENTITY, &rect);
            scene.draw_image(brush, display);
            scene.pop_layer();
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
            ctx.create_pod(ImageViewWidget::new(
                self.image.clone(),
                self.img_size,
                self.fit,
            )),
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
        _ctx: &mut ViewCtx,
        _element: Mut<'_, Self::Element>,
    ) {
    }

    fn message(
        &self,
        _vs: &mut Self::ViewState,
        message: &mut MessageContext,
        _element: Mut<'_, Self::Element>,
        state: &mut AppState,
    ) -> MessageResult<()> {
        if let Some(action) = message.take_message::<ImageViewAction>() {
            match *action {
                ImageViewAction::NavigateLeft => state.navigate_left(),
                ImageViewAction::NavigateRight => state.navigate_right(),
                ImageViewAction::Exit => state.exit_slideshow(),
            }
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
    let app = Xilem::new_simple(
        state,
        app_view,
        WindowOptions::new("Image_Loader")
            .with_initial_inner_size(LogicalSize::new(1280.0, 768.0)),
    );
    if let Err(e) = app.run_in(event_loop) {
        eprintln!("应用退出：{e}");
    }
}
