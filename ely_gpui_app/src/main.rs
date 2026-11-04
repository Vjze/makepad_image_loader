//! Image Loader —— 基于 ely-gpui 组件库复刻 Makepad 版的功能与布局。
//!
//! 复用 `image_loader_core`（框架无关的目录流式扫描 / 文件树），图片显示与交互交给
//! GPUI / ely-gpui：
//!
//! - 选择文件夹后在**后台线程**流式扫描，每发现一张图片就实时追加进缩略图列表，
//!   顶部有进度条 + Spinner + 文本进度（先统计总数，再按目录项推进真实百分比）。
//! - 缩略图由后台 2 个 worker 解码并下采样成 `Arc<RenderImage>`（BGRA8），带 FIFO 上限的
//!   内存缓存；渲染时用 `canvas` + `window.paint_image` **同步绘制**。不用 `img()` 元素：
//!   它在 layout 阶段会强制注入 `aspect_ratio`，与卡片里的百分比尺寸组合会把绘制区域
//!   算成 0，导致缩略图始终空白。
//! - 幻灯片大图用 `ely_media::ImageViewer`。Esc / 方向键在 `App` 层注册**全局** action
//!   监听（不依赖焦点）；退出按钮放在独立顶部工具条，不与 viewer 自带的旋转按钮重叠。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use ely_core::buttons::{Button, IconButton};
use ely_core::layout::{ScrollArea, Sidebar};
use ely_core::lists::VirtualList;
use ely_core::motion::{ProgressBar, Spinner};
use ely_core::primitives::{Icon, IconName};
use ely_core::theme::{ActiveTheme, ControlSize, IconSize, Mode, Theme};
use ely_media::ImageViewer;
use image_loader_core::scanner::{
    FileTreeNode, ScanEvent, count_entries, scan_directory_streaming,
};

use gpui::{
    AnyElement, App, Bounds, Context, Corners, ElementId, Entity, Hsla, IntoElement, KeyBinding,
    ObjectFit, Render, RenderImage, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, canvas, div, px, rgb, size,
};
use gpui::prelude::*;

// 全局键盘动作：Esc 退出幻灯片，左右方向键切换图片。
actions!(app, [Escape, ArrowLeft, ArrowRight]);

/// 缩略图最长边上限（像素）。原图往往几千万像素，直接解码会把内存撑爆。
const THUMB_MAX_DIM: u32 = 256;
/// 解码后缩略图的内存缓存上限（约 256×256×4 = 256KB/张，400 张 ≈ 100MB 上限）。
const THUMB_CACHE_CAP: usize = 400;
/// 网格卡片尺寸与间距。卡片是 `flex_col`：上面固定图片区、下面固定文件名条，
/// 避免 absolute + 百分比尺寸在 taffy 里被算成 0 高度。
///
/// `CELL_W` 只是**求列数用的目标宽度**：实际卡片宽度由 `grid_layout()` 按可用
/// 宽度等分得出（`>= CELL_W`），这样每行最后一张卡片后面不会留下接近一整列的
/// 空白。固定 200px 时窗口右侧常余 100~200px，看起来就像凭空少了一列。
const CELL_W: f32 = 200.0;
const IMAGE_H: f32 = 122.0;
const CAPTION_H: f32 = 28.0;
const CARD_H: f32 = IMAGE_H + CAPTION_H;
const GAP: f32 = 12.0;
/// 估算可用宽度时预留给纵向滚动条 / 窗口边框的余量。留太少会多算一列，
/// 行内放不下导致最后一张被裁。
const SCROLLBAR_RESERVE: f32 = 10.0;
/// 网格行左右两侧的内边距。余量必须**均分到两侧**（而不是整块堆在右边），
/// 否则右侧边距会比左侧大 `SCROLLBAR_RESERVE`，视觉上明显不对称。
const SIDE_PAD: f32 = GAP + SCROLLBAR_RESERVE / 2.0;
/// VirtualList 每行的精确高度（卡片 + 顶部间距），显式给高度避免行测量坍缩。
const ROW_H: f32 = CARD_H + GAP;
/// 后台事件泵轮询间隔：扫描进度与「缩略图就绪」都靠它驱动重绘。
const PUMP_INTERVAL: Duration = Duration::from_millis(40);

// ---------------------------------------------------------------------------
// 缩略图：后台解码 + 有界缓存 + 同步渲染
// ---------------------------------------------------------------------------

/// 一次缩略图查询的三种结果。
enum ThumbResult {
    Ready(Arc<RenderImage>),
    Failed,
    Pending,
}

#[derive(Default)]
struct ThumbState {
    /// `None` 表示该文件解码失败（缓存住，避免反复重试）。
    cache: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    /// 已入队但尚未完成的路径，避免重复入队。
    pending: HashSet<PathBuf>,
    queue: VecDeque<PathBuf>,
    /// 简单的 FIFO 淘汰顺序。
    order: VecDeque<PathBuf>,
}

struct ThumbLoader {
    state: Mutex<ThumbState>,
    cond: Condvar,
}

/// 两个常驻 worker 线程，专门做「读原图 → 下采样 → 转 BGRA」的重活，
/// 完全不占 UI 线程。
static THUMBS: LazyLock<Arc<ThumbLoader>> = LazyLock::new(|| {
    let loader = Arc::new(ThumbLoader {
        state: Mutex::new(ThumbState::default()),
        cond: Condvar::new(),
    });
    for _ in 0..2 {
        let worker = loader.clone();
        std::thread::spawn(move || thumb_worker(worker));
    }
    loader
});

fn thumb_worker(loader: Arc<ThumbLoader>) {
    loop {
        let path = {
            let mut st = loader.state.lock().unwrap();
            loop {
                if let Some(p) = st.queue.pop_front() {
                    break p;
                }
                st = loader.cond.wait(st).unwrap();
            }
        };

        let decoded = decode_thumb(&path);
        {
            let mut st = loader.state.lock().unwrap();
            st.pending.remove(&path);
            st.order.push_back(path.clone());
            st.cache.insert(path.clone(), decoded);
            while st.cache.len() > THUMB_CACHE_CAP {
                let Some(old) = st.order.pop_front() else { break };
                st.cache.remove(&old);
            }
        }
        // 用一个原子标记通知 UI 重绘，不走 channel：大文件夹里每张图一条消息会
        // 淹没扫描进度事件，而且一旦 channel 未初始化就会永久丢通知。
        THUMB_DIRTY.store(true, Ordering::Release);
    }
}

/// 有缩略图解码完成、UI 需要重绘的标记。由事件泵每 40ms 消费一次。
static THUMB_DIRTY: AtomicBool = AtomicBool::new(false);

/// 把原图解码、下采样到 `THUMB_MAX_DIM` 以内，并转成 gpui 约定的 BGRA8。
fn decode_thumb(path: &Path) -> Option<Arc<RenderImage>> {
    let img = image::open(path).ok()?;
    // `thumbnail()` 会把小图放大到边界框（例如 4×2 → 256×128），纯属浪费；
    // 只有确实超过上限才下采样，小图原样使用。
    let small = if img.width() > THUMB_MAX_DIM || img.height() > THUMB_MAX_DIM {
        img.thumbnail(THUMB_MAX_DIM, THUMB_MAX_DIM)
    } else {
        img
    };
    let mut rgba = small.to_rgba8();
    // gpui 的 RenderImage 内部是 BGRA（见 platform::decode_static_image）。
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

/// 查询缩略图：命中缓存直接返回；未命中则丢给后台 worker，本次先渲染 loading 占位。
fn thumb_for(path: &Path) -> ThumbResult {
    let loader: Arc<ThumbLoader> = (*THUMBS).clone();
    let key = path.to_path_buf();
    let mut st = loader.state.lock().unwrap();
    match st.cache.get(&key) {
        Some(Some(data)) => return ThumbResult::Ready(data.clone()),
        Some(None) => return ThumbResult::Failed,
        None => {}
    }
    if !st.pending.contains(&key) {
        st.pending.insert(key.clone());
        st.queue.push_back(key);
        loader.cond.notify_one();
    }
    ThumbResult::Pending
}

// ---------------------------------------------------------------------------
// 后台事件通道：扫描流式事件 + 缩略图就绪通知，统一由一个前台事件泵消费
// ---------------------------------------------------------------------------

enum WorkerEvent {
    ScanTotal { generation: u64, total: usize },
    ScanProgress { generation: u64, visited: usize, current: String },
    ScanFound { generation: u64, index: usize, path: PathBuf },
    ScanDone { generation: u64, root: FileTreeNode },
}

static EVENT_TX: OnceLock<Mutex<Sender<WorkerEvent>>> = OnceLock::new();
static SCAN_GEN: AtomicU64 = AtomicU64::new(0);

fn emit(event: WorkerEvent) -> bool {
    match EVENT_TX.get() {
        Some(tx) => tx.lock().unwrap().send(event).is_ok(),
        None => false,
    }
}

/// 全局状态（存放在 `Entity` 中，使全局 action 与各视图都能读写）。
#[derive(Clone, Default)]
struct AppState {
    image_paths: Vec<PathBuf>,
    filtered_idxs: Vec<usize>,
    file_tree: Option<FileTreeNode>,
    /// 当前图片在 `filtered_idxs` 中的位置（None 表示未选中）。
    current_image_idx: Option<usize>,
    in_slideshow: bool,
    sidebar_collapsed: bool,
    /// 已折叠的文件夹路径集合（文件树）。
    collapsed: HashSet<String>,
    scan: ScanState,
    /// 当前扫描的代次；旧扫描线程的迟到事件会被丢弃。
    scan_gen: u64,
}

/// 扫描进度（供进度条 / loading 组件读取）。
#[derive(Clone, Default, Debug)]
struct ScanState {
    active: bool,
    /// 第一阶段：正在统计目录项总数（此时用不确定进度条）。
    counting: bool,
    visited: usize,
    total: usize,
    found: usize,
    current: String,
}

/// 把一条后台事件应用到状态上，返回是否需要重绘。
fn apply_worker_event(s: &mut AppState, event: WorkerEvent) -> bool {
    match event {
        WorkerEvent::ScanTotal { generation, total } => {
            if generation != s.scan_gen {
                return false;
            }
            s.scan.total = total;
            s.scan.counting = false;
            true
        }
        WorkerEvent::ScanProgress { generation, visited, current } => {
            if generation != s.scan_gen {
                return false;
            }
            s.scan.visited = visited;
            s.scan.current = current;
            true
        }
        WorkerEvent::ScanFound { generation, index, path } => {
            if generation != s.scan_gen {
                return false;
            }
            // 事件严格按发现顺序到达；index 就是 image_paths 的下标。
            if index == s.image_paths.len() {
                s.image_paths.push(path);
                s.filtered_idxs.push(index);
                s.scan.found = s.image_paths.len();
            }
            true
        }
        WorkerEvent::ScanDone { generation, root } => {
            if generation != s.scan_gen {
                return false;
            }
            s.file_tree = Some(root);
            s.scan.active = false;
            s.scan.counting = false;
            s.scan.visited = s.scan.visited.max(s.scan.total);
            s.scan.current.clear();
            true
        }
    }
}

// ---------------------------------------------------------------------------
// 根视图
// ---------------------------------------------------------------------------

struct RootView {
    model: Entity<AppState>,
}

impl RootView {
    fn new(cx: &mut Context<Self>, model: Entity<AppState>) -> Self {
        // 模型变化时重绘本视图（观察者是 RootView 自身）。
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self { model }
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 本次重绘已经发生，可以清除"缩略图解码完成"标记。由 render 清除而不是
        // 事件泵读取时清除，即使某次 notify 丢失，事件泵也会持续触发重绘，
        // 卡片不会永远停在 Spinner。
        THUMB_DIRTY.store(false, Ordering::Release);
        // 用 Arc 包一层：VirtualList 的行闭包只需克隆 Arc，流式扫描时每 40ms
        // 重绘一次，避免反复深拷贝整份 image_paths / filtered_idxs。
        let state = Arc::new(self.model.read(cx).clone());
        let colors = &cx.theme().colors;
        let model = self.model.clone();

        let content = if state.in_slideshow {
            build_slideshow(&state, model.clone(), cx)
        } else {
            build_browser(state.clone(), model.clone(), window, cx)
        };

        div()
            .size_full()
            .relative()
            .flex()
            .bg(colors.bg)
            // 焦点落在窗口内元素时，element 级 on_action 能捕获冒泡上来的 action。
            // 焦点为空时派发路径只有窗口根节点，够不到这里 —— 那种情况由 launch()
            // 里注册的 App 级全局 on_action 兜底（两条路径互斥，不会重复触发）。
            .on_action({
                let m = model.clone();
                move |_: &Escape, _window, cx| {
                    m.update(cx, |s, cx| {
                        if s.in_slideshow {
                            s.in_slideshow = false;
                            cx.notify();
                        }
                    });
                }
            })
            .on_action({
                let m = model.clone();
                move |_: &ArrowLeft, _window, cx| {
                    m.update(cx, |s, cx| {
                        if s.in_slideshow {
                            navigate(s, -1);
                            cx.notify();
                        }
                    });
                }
            })
            .on_action({
                let m = model.clone();
                move |_: &ArrowRight, _window, cx| {
                    m.update(cx, |s, cx| {
                        if s.in_slideshow {
                            navigate(s, 1);
                            cx.notify();
                        }
                    });
                }
            })
            // 三色径向渐变光斑（青 / 金 / 紫），复刻 Makepad 版背景氛围。
            .child(background_blobs())
            .when(!state.sidebar_collapsed, |row| {
                row.child(
                    Sidebar::new("sidebar", false)
                        .child(build_sidebar(&state, model.clone(), cx)),
                )
            })
            .child(content)
    }
}

/// 浏览器主页：顶部工具栏 + 扫描进度条 + 缩略图网格。
fn build_browser(
    state: Arc<AppState>,
    model: Entity<AppState>,
    window: &Window,
    cx: &Context<RootView>,
) -> AnyElement {
    let colors = &cx.theme().colors;
    let n = state.filtered_idxs.len();

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(
            // 顶部工具栏。
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(colors.border)
                .child(
                    Button::new("open", "打开文件夹").on_click({
                        let model = model.clone();
                        move |_, _, cx| open_folder(model.clone(), cx)
                    }),
                )
                .child(
                    div()
                        .text_color(colors.fg_muted)
                        .text_size(px(13.0))
                        .child(SharedString::from(if state.scan.active {
                            format!("共: {} 张（读取中…）", n)
                        } else {
                            format!("共: {} 张", n)
                        })),
                )
                .when(n > 0, |bar| {
                    bar.child(
                        Button::new("slideshow", "幻灯片模式").on_click({
                            let model = model.clone();
                            move |_, _, cx| {
                                model.update(cx, |s, cx| {
                                    if s.current_image_idx.is_none() {
                                        s.current_image_idx = Some(0);
                                    }
                                    s.in_slideshow = true;
                                    cx.notify();
                                });
                            }
                        }),
                    )
                })
                .child(
                    Button::new(
                        "toggle-sidebar",
                        if state.sidebar_collapsed { "显示列表" } else { "隐藏列表" },
                    )
                    .on_click({
                        let model = model.clone();
                        move |_, _, cx| {
                            model.update(cx, |s, cx| {
                                s.sidebar_collapsed = !s.sidebar_collapsed;
                                cx.notify();
                            });
                        }
                    }),
                ),
        )
        // 扫描中显示进度条 + Spinner；结果是边扫边写入列表的，所以进度条下方
        // 的网格同时会不断冒出新缩略图。
        .when(state.scan.active, |root| {
            root.child(build_scan_progress(&state.scan, cx))
        })
        .child(build_grid(state, model, window, cx))
        .into_any_element()
}

/// 扫描进度条：统计阶段用不确定进度条，扫描阶段用真实百分比。
fn build_scan_progress(scan: &ScanState, cx: &Context<RootView>) -> AnyElement {
    let colors = &cx.theme().colors;
    let ratio = if scan.total > 0 {
        (scan.visited as f32 / scan.total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let label = if scan.counting {
        "正在统计文件数量…".to_string()
    } else {
        format!(
            "正在扫描 {} — 已检查 {}/{} 项，已找到 {} 张图片（结果实时写入列表）",
            scan.current, scan.visited, scan.total, scan.found
        )
    };
    let percent = format!("{}%", (ratio * 100.0) as u32);
    let bar = if scan.counting || scan.total == 0 {
        ProgressBar::indeterminate("scan-progress").into_any_element()
    } else {
        ProgressBar::new("scan-progress", ratio).into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .px(px(12.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(colors.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(Spinner::new("scan-spinner").size(IconSize::Sm))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(colors.fg_muted)
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(colors.fg_muted)
                        .child(SharedString::from(if scan.counting {
                            "…".to_string()
                        } else {
                            percent
                        })),
                ),
        )
        .child(div().w_full().child(bar))
        .into_any_element()
}

/// 依据内容区宽度算「每行几张 + 每张多宽」。
///
/// 固定卡片宽度时，窗口右侧常剩下 100~200px（不够再放一张），视觉上就是
///「最右边空了一列」。这里先按目标宽度 `CELL_W` 求列数，再把剩余空间
/// 均摊到每张卡片上，保证 `cols * card_w + (cols-1) * GAP == 可用宽度`，
/// 即整行恰好铺满。返回的 `card_w` 恒 `>= CELL_W`，卡片只会被拉宽、不会变窄。
fn grid_layout(content_w: f32) -> (usize, f32) {
    // 左右各扣 SIDE_PAD（= GAP + 一半余量），保证左右边距相等；宁可少算一列
    // 也不要多算（多算会让一行放不下，最后一张被裁）。
    let inner_w = (content_w - 2.0 * SIDE_PAD).max(CELL_W);
    let cols = (((inner_w + GAP) / (CELL_W + GAP)).floor().max(1.0)) as usize;
    let card_w = (inner_w - (cols - 1) as f32 * GAP) / cols as f32;
    (cols, card_w)
}

/// 缩略图网格：用 ely 的 `VirtualList` 做虚拟滚动（只构建可见行）。
/// 列表内容来自后台流式扫描，因此在扫描过程中会持续增长；每张卡片用后台解码出的
/// `RenderImage` 同步绘制，未就绪时显示 Spinner 占位，失败时显示图标。
fn build_grid(
    state: Arc<AppState>,
    model: Entity<AppState>,
    window: &Window,
    cx: &Context<RootView>,
) -> AnyElement {
    let n = state.filtered_idxs.len();
    if n == 0 {
        let hint = if state.scan.active {
            "正在读取图片，发现的图片会实时写入列表…"
        } else {
            "打开文件夹以加载图片"
        };
        return div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .when(state.scan.active, |d| {
                        d.child(Spinner::new("grid-spinner"))
                    })
                    .child(
                        div()
                            .text_color(cx.theme().colors.fg_muted)
                            .text_size(px(14.0))
                            .child(SharedString::from(hint)),
                    ),
            )
            .into_any_element();
    }

    // 内容区真实宽度 = 窗口 viewport 宽度 - 侧栏宽度 - 侧栏 1px 右边框。
    // 侧栏折叠时根视图根本不渲染 Sidebar，此时不能再扣它的宽度（否则右侧会
    // 凭空多出 56px 空隙）。用 viewport_size() 而不是 bounds()：后者是窗口
    // 外框尺寸，含系统边框/标题栏，用来算列数会偏大并导致最后一张被裁。
    let theme = cx.theme();
    let sidebar_px = if state.sidebar_collapsed {
        0.0
    } else {
        theme.sidebar_width(false).0 * window.rem_size().as_f32() + 1.0
    };
    let content_w = (window.viewport_size().width.as_f32() - sidebar_px).max(CELL_W);
    // 列数 + 实际卡片宽度：卡片等宽拉伸铺满整行，右侧不再留下"空一列"。
    let (cols, card_w) = grid_layout(content_w);
    let rows = (n + cols - 1) / cols;

    let state2 = state.clone();
    let model2 = model.clone();

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            VirtualList::new("grid", rows, move |row_ix, _win, cx| {
                let start = row_ix * cols;
                let end = (start + cols).min(state2.filtered_idxs.len());
                let full_row = end - start == cols;
                // 显式行高 + 显式主轴方向：div() 默认 Row，若容器漏了 flex_col，
                // VirtualList 的 size_full() 会拿不到确定高度，整列都不会渲染。
                // 不用 flex_wrap：每行固定 cols 张、每张宽度已精确算好，换行只会
                // 把多出来的那一行裁掉（行高是固定的）。
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .h(px(ROW_H))
                    .gap(px(GAP))
                    // 左右内边距相同；满行再把可能的几 px 估算误差居中均分，
                    // 保证左右视觉边距严格一致。最后一行不满时保持左对齐。
                    .px(px(SIDE_PAD))
                    .when(full_row, |d| d.justify_center())
                    .pt(px(GAP))
                    .children(
                        (start..end)
                            .map(|idx| {
                                let raw_idx = state2.filtered_idxs[idx];
                                let path = state2.image_paths[raw_idx].clone();
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                let selected = state2.current_image_idx == Some(idx);
                                build_thumbnail_card(
                                    idx,
                                    path,
                                    name,
                                    selected,
                                    card_w,
                                    model2.clone(),
                                    cx,
                                )
                            })
                            .collect::<Vec<AnyElement>>(),
                    )
                    .into_any_element()
            })
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0(),
        )
        .into_any_element()
}

/// 单张缩略图卡片。图片用 `canvas` + `window.paint_image` 同步绘制：绘制区域就是
/// canvas 自己的 bounds（由卡片宽 `card_w` 与固定图片高度给定），不经过 `img()`
/// 元素在 layout 阶段注入的 `aspect_ratio`，因此不会出现"算成 0 尺寸、什么都不画"。
/// `card_w` 来自 `grid_layout()`，是随窗口宽度浮动的等分宽度，不是常量。
/// 未解码完成时显示 Spinner，解码失败显示图标。
///
/// 注意：选中描边用主链上的 `.border_1()` + `.border_color(if ...)`，不要写进
/// `.when()` 闭包里 —— gpui-pre 0.3.10 下 `.when(|d| d.border_1())` 会让链尾
/// `.on_click()` 解析失败（E0599）。
fn build_thumbnail_card(
    idx: usize,
    path: PathBuf,
    name: String,
    selected: bool,
    card_w: f32,
    model: Entity<AppState>,
    cx: &App,
) -> AnyElement {
    let colors = &cx.theme().colors;
    let model_cell = model.clone();

    let picture: AnyElement = match thumb_for(&path) {
        ThumbResult::Ready(data) => canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let img_size = data.size(0);
                if u32::from(img_size.width) == 0 || u32::from(img_size.height) == 0 {
                    return;
                }
                // Cover 只用来算等比裁剪矩形；paint_image 会把它与 bounds 求交，
                // 超出卡片的部分自动裁掉。
                let image_bounds = ObjectFit::Cover.get_bounds(bounds, img_size);
                let _ = window.paint_image(
                    bounds,
                    image_bounds,
                    Corners::default(),
                    data.clone(),
                    0,
                    false,
                );
            },
        )
        .w(px(card_w))
        .h(px(IMAGE_H))
        .flex_none()
        .into_any_element(),
        ThumbResult::Pending => div()
            .w(px(card_w))
            .h(px(IMAGE_H))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors.sunken)
            .child(Spinner::new(ElementId::from(format!("thumb-spin-{idx}"))).size(IconSize::Sm))
            .into_any_element(),
        ThumbResult::Failed => div()
            .w(px(card_w))
            .h(px(IMAGE_H))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors.sunken)
            .child(
                Icon::new(IconName::ImageOff)
                    .size(IconSize::Lg)
                    .color(colors.fg_muted),
            )
            .into_any_element(),
    };

    let caption = div()
        .w_full()
        .h(px(CAPTION_H))
        .flex_none()
        .px(px(6.0))
        .flex()
        .items_center()
        .bg(colors.bg)
        .text_color(colors.fg_muted)
        .text_size(px(11.0))
        .truncate()
        .child(SharedString::from(name));

    let card = div()
        .id(ElementId::from(format!("thumb-{idx}")))
        .relative()
        .flex()
        .flex_col()
        .w(px(card_w))
        .flex_none()
        .h(px(CARD_H))
        .rounded(px(8.0))
        .overflow_hidden()
        .bg(colors.surface)
        .border_1()
        .border_color(if selected { colors.accent } else { colors.border })
        .child(picture)
        .child(caption);

    card
        .on_click(move |_, _, cx| {
            model_cell.update(cx, |s, cx| {
                s.current_image_idx = Some(idx);
                s.in_slideshow = true;
                cx.notify();
            });
        })
        .into_any_element()
}

/// 侧边栏：标题 + 可滚动的文件树。
fn build_sidebar(
    state: &AppState,
    model: Entity<AppState>,
    cx: &Context<RootView>,
) -> impl IntoElement {
    let colors = &cx.theme().colors;
    let tree_children: Vec<AnyElement> =
        state.file_tree.as_ref().map_or_else(Vec::new, |tree| {
            build_tree_nodes(tree, state, model.clone(), cx, 0)
        });

    div()
        .flex()
        .flex_col()
        .size_full()
        .child(
            div()
                .px(px(10.0))
                .py(px(8.0))
                .text_color(colors.fg)
                .text_size(px(11.0))
                .child(SharedString::from("文件列表")),
        )
        .child(
            ScrollArea::new("tree-scroll").vertical().child(
                div()
                    .flex_col()
                    .children(tree_children)
                    // 文件树在扫描结束后一次性构建（与 image_idx 保持一致），
                    // 扫描期间先给一个明确的 loading 提示。
                    .when(state.scan.active && state.file_tree.is_none(), |d| {
                        d.child(
                            div()
                                .px(px(10.0))
                                .py(px(6.0))
                                .text_size(px(11.0))
                                .text_color(colors.fg_muted)
                                .child(SharedString::from("正在读取目录结构…")),
                        )
                    }),
            ),
        )
}

/// 递归渲染文件树（文件夹可折叠，文件可点击打开）。
fn build_tree_nodes(
    node: &FileTreeNode,
    state: &AppState,
    model: Entity<AppState>,
    cx: &Context<RootView>,
    depth: usize,
) -> Vec<AnyElement> {
    let colors = &cx.theme().colors;
    let indent = px(8.0 + depth as f32 * 14.0);
    let mut out: Vec<AnyElement> = Vec::new();

    if let Some(raw_idx) = node.image_idx {
        // 文件节点
        let name = node.name.clone();
        let active = state
            .current_image_idx
            .and_then(|p| state.filtered_idxs.get(p).copied())
            == Some(raw_idx);
        out.push(
            div()
                .id(ElementId::from(format!("file-{raw_idx}")))
                .px(indent)
                .py(px(3.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .when(active, |d| d.bg(colors.selection))
                .hover(|d| d.bg(colors.hover))
                .child(
                    Icon::new(IconName::File)
                        .size(IconSize::Sm)
                        .color(colors.fg_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(colors.fg)
                        .text_size(px(12.0))
                        .child(SharedString::from(name)),
                )
                .on_click(move |_, _, cx| {
                    model.update(cx, |s, cx| {
                        if let Some(pos) = s.filtered_idxs.iter().position(|&i| i == raw_idx) {
                            s.current_image_idx = Some(pos);
                            s.in_slideshow = true;
                            cx.notify();
                        }
                    });
                })
                .into_any_element(),
        );
    } else {
        // 文件夹节点
        let key = node.path.clone();
        let is_open = !state.collapsed.contains(&key);
        let model_for_click = model.clone();
        out.push(
            div()
                .id(ElementId::from(format!("folder-{}", key)))
                .px(indent)
                .py(px(3.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .hover(|d| d.bg(colors.hover))
                .child(
                    Icon::new(if is_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(IconSize::Sm)
                    .color(colors.fg_muted),
                )
                .child(
                    Icon::new(if is_open {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                    .size(IconSize::Sm)
                    .color(colors.fg_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(colors.fg)
                        .text_size(px(12.0))
                        .child(SharedString::from(node.name.clone())),
                )
                .on_click(move |_, _, cx| {
                    model_for_click.update(cx, |s, cx| {
                        if s.collapsed.contains(&key) {
                            s.collapsed.remove(&key);
                        } else {
                            s.collapsed.insert(key.clone());
                        }
                        cx.notify();
                    });
                })
                .into_any_element(),
        );
        if is_open {
            for child in &node.children {
                out.extend(build_tree_nodes(child, state, model.clone(), cx, depth + 1));
            }
        }
    }
    out
}

/// 幻灯片模式：独立顶部工具条 + 全屏大图（ImageViewer）。
///
/// 退出按钮与上一张/下一张按钮放在 viewer **之外** 的顶部独立一行：ImageViewer
/// 自带一条 toolbar（左侧缩放、右侧左右旋转），之前把 X 按钮绝对定位到
/// `.top_0().right_0()` 正好压在旋转按钮上。
fn build_slideshow(
    state: &AppState,
    model: Entity<AppState>,
    cx: &Context<RootView>,
) -> AnyElement {
    let colors = &cx.theme().colors;
    let cur = state.current_image_idx;
    let n = state.filtered_idxs.len();
    let pos = cur.unwrap_or(0);
    let raw_idx = cur.and_then(|p| state.filtered_idxs.get(p).copied());
    let name = raw_idx
        .map(|i| {
            state.image_paths[i]
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    let has_prev = pos > 0;
    let has_next = pos + 1 < n;

    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child(
            IconButton::new("exit-slideshow", IconName::X)
                .size(ControlSize::Lg)
                .on_click({
                    let m = model.clone();
                    move |_, _, cx| {
                        m.update(cx, |s, cx| {
                            s.in_slideshow = false;
                            cx.notify();
                        });
                    }
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(13.0))
                .text_color(colors.fg)
                .child(SharedString::from(name)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(colors.fg_muted)
                .child(SharedString::from(format!("{} / {}", pos + 1, n))),
        )
        .child(
            IconButton::new("prev-image", IconName::ChevronLeft)
                .size(ControlSize::Lg)
                .disabled(!has_prev)
                .on_click({
                    let m = model.clone();
                    move |_, _, cx| {
                        m.update(cx, |s, cx| {
                            navigate(s, -1);
                            cx.notify();
                        });
                    }
                }),
        )
        .child(
            IconButton::new("next-image", IconName::ChevronRight)
                .size(ControlSize::Lg)
                .disabled(!has_next)
                .on_click({
                    let m = model.clone();
                    move |_, _, cx| {
                        m.update(cx, |s, cx| {
                            navigate(s, 1);
                            cx.notify();
                        });
                    }
                }),
        );

    let body = div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .when_some(raw_idx, |body, raw_idx| {
            let path = state.image_paths[raw_idx].clone();
            body
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(ImageViewer::new("viewer", path)),
                )
                // 底部操作提示。
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .py(px(8.0))
                        .flex()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(colors.fg_muted)
                        .child(SharedString::from(
                            "Esc 退出 · 上一张/下一张切换 · 滚轮缩放 / 拖动平移",
                        )),
                )
        });

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .bg(colors.bg)
        .child(header)
        .child(body)
        .into_any_element()
}

/// 背景三色径向渐变光斑（复刻 Makepad 版氛围）：青 / 金 / 紫。
fn background_blobs() -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top(px(-120.0))
                .left(px(-120.0))
                .size(px(440.0))
                .rounded_full()
                .bg(Hsla::from(rgb(0x22d3ee)).opacity(0.10)),
        )
        .child(
            div()
                .absolute()
                .top(px(-100.0))
                .right(px(-100.0))
                .size(px(400.0))
                .rounded_full()
                .bg(Hsla::from(rgb(0xfbbf24)).opacity(0.08)),
        )
        .child(
            div()
                .absolute()
                .bottom(px(-180.0))
                .left(px(160.0))
                .size(px(480.0))
                .rounded_full()
                .bg(Hsla::from(rgb(0xa78bfa)).opacity(0.10)),
        )
}

/// 打开文件夹对话框（模态），选中后立即启动后台流式扫描。
fn open_folder(model: Entity<AppState>, cx: &mut App) {
    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
        start_scan(folder, model, cx);
    }
}

/// 启动一次后台扫描：先统计总数，再流式扫描；每发现一张图片就发一条消息，
/// UI 事件泵收到后立刻追加进列表（边读边写），并同步刷新进度条。
fn start_scan(folder: PathBuf, model: Entity<AppState>, cx: &mut App) {
    let generation = SCAN_GEN.fetch_add(1, Ordering::Relaxed) + 1;

    model.update(cx, |s, cx| {
        s.scan_gen = generation;
        s.scan = ScanState {
            active: true,
            counting: true,
            ..Default::default()
        };
        s.image_paths.clear();
        s.filtered_idxs.clear();
        s.file_tree = None;
        s.current_image_idx = None;
        s.in_slideshow = false;
        s.collapsed.clear();
        cx.notify();
    });

    std::thread::spawn(move || {
        let total = count_entries(&folder);
        if !emit(WorkerEvent::ScanTotal { generation, total }) {
            return;
        }
        let mut last_progress = 0usize;
        let root = scan_directory_streaming(&folder, &mut |event| {
            match event {
                ScanEvent::Progress { visited, current } => {
                    // 进度消息节流：每 16 个目录项上报一次，避免海量消息淹没 UI。
                    if visited.saturating_sub(last_progress) < 16 && visited != 1 {
                        return;
                    }
                    last_progress = visited;
                    emit(WorkerEvent::ScanProgress {
                        generation,
                        visited,
                        current,
                    });
                }
                // Found 不节流：每一张都要实时写进列表。
                ScanEvent::Found { index, path } => {
                    emit(WorkerEvent::ScanFound { generation, index, path });
                }
            }
        });
        emit(WorkerEvent::ScanDone { generation, root });
    });
}

/// 在 filtered 图片列表内按 delta 切换当前图片。
fn navigate(s: &mut AppState, delta: i32) {
    if let Some(i) = s.current_image_idx {
        let n = s.filtered_idxs.len() as i32;
        let ni = (i as i32 + delta).clamp(0, n - 1);
        if (ni as usize) != i {
            s.current_image_idx = Some(ni as usize);
        }
    }
}

/// 应用入口：初始化 Ely + 主题，绑定快捷键与全局 action，启动后台事件泵，打开窗口。
fn launch(cx: &mut App) -> WindowHandle<RootView> {
    ely_core::init(cx).expect("Ely failed to start");
    Theme::set_mode_now(Mode::Dark, cx);

    let model = cx.new(|_| AppState::default());

    let bounds = Bounds::centered(None, size(px(1280.0), px(768.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Image_Loader".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    cx.bind_keys([
        KeyBinding::new("escape", Escape, None),
        KeyBinding::new("left", ArrowLeft, None),
        KeyBinding::new("right", ArrowRight, None),
    ]);

    // App 级全局 action 监听：不依赖焦点。窗口没有焦点元素时，按键 action 只派发到
    // 窗口根节点，RootView 的根 div 不在那条 dispatch_path 上，只有全局监听能收到。
    // 焦点落在窗口内元素时，根 div 的 element 级监听会在 bubble 阶段截住并停止传播，
    // 因此两条路径互斥，不会重复切图 / 重复退出。
    {
        let m = model.clone();
        cx.on_action(move |_: &Escape, cx: &mut App| {
            m.update(cx, |s, cx| {
                if s.in_slideshow {
                    s.in_slideshow = false;
                    cx.notify();
                }
            });
        });
    }
    {
        let m = model.clone();
        cx.on_action(move |_: &ArrowLeft, cx: &mut App| {
            m.update(cx, |s, cx| {
                if s.in_slideshow {
                    navigate(s, -1);
                    cx.notify();
                }
            });
        });
    }
    {
        let m = model.clone();
        cx.on_action(move |_: &ArrowRight, cx: &mut App| {
            m.update(cx, |s, cx| {
                if s.in_slideshow {
                    navigate(s, 1);
                    cx.notify();
                }
            });
        });
    }

    // 后台事件泵：把扫描线程 / 缩略图 worker 的消息批量取回，在前台更新状态并重绘。
    let (tx, rx) = channel::<WorkerEvent>();
    let _ = EVENT_TX.set(Mutex::new(tx));
    let pump_model = model.clone();
    cx.spawn(async move |cx| {
        loop {
            let mut batch = Vec::new();
            while let Ok(event) = rx.try_recv() {
                batch.push(event);
            }
            // 缩略图就绪用原子标记驱动重绘，与扫描事件合并到同一帧，
            // 保证 worker 解码完成后卡片一定会从 Spinner 换成真实图片。
            let thumb_dirty = THUMB_DIRTY.load(Ordering::Acquire);
            if !batch.is_empty() || thumb_dirty {
                cx.update(|cx: &mut App| {
                    pump_model.update(cx, |s, cx| {
                        let mut changed = thumb_dirty;
                        for event in batch {
                            changed |= apply_worker_event(s, event);
                        }
                        if changed {
                            cx.notify();
                        }
                    });
                });
            }
            cx.background_executor().timer(PUMP_INTERVAL).await;
        }
    })
    .detach();

    let window = cx
        .open_window(options, |_window, cx| {
            cx.new(|cx| RootView::new(cx, model.clone()))
        })
        .expect("open window");

    cx.activate(true);
    window
}

fn main() {
    gpui_platform::application()
        .with_assets(ely_core::Assets)
        .run(|cx: &mut App| {
            launch(cx);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证后台解码链路能产出 gpui 可直接绘制的 BGRA `RenderImage`。
    #[test]
    fn decode_thumb_produces_a_drawable_frame() {
        let mut src: image::RgbaImage = image::RgbaImage::new(4, 2);
        for (_x, _y, px) in src.enumerate_pixels_mut() {
            *px = image::Rgba([10, 20, 30, 255]);
        }
        let mut path = std::env::temp_dir();
        path.push("ely_thumb_decode_test.png");
        src.save(&path).expect("write test png");

        let decoded = decode_thumb(&path).expect("decode_thumb should succeed");
        assert_eq!(decoded.frame_count(), 1);
        assert_eq!(decoded.as_bytes(0).unwrap().len(), 4 * 2 * 4);
        // 每个像素都是 RGBA(10,20,30,255)，应被转成 BGRA(30,20,10,255)。
        assert_eq!(&decoded.as_bytes(0).unwrap()[..4], &[30, 20, 10, 255]);

        let _ = std::fs::remove_file(&path);
    }

    /// canvas 绘制路径：Cover 算出的区域与卡片 bounds 求交后必须非空。
    /// 之前用 `img()` + `.absolute().inset_0().size_full()` 时，绘制区域会被算成
    /// 0 尺寸（paint_image 直接 return），表现就是缩略图永远空白。
    #[test]
    fn thumbnail_canvas_has_non_empty_paint_area() {
        use gpui::point;

        let mut src = image::RgbaImage::new(320, 200);
        for (_x, _y, pixel) in src.enumerate_pixels_mut() {
            *pixel = image::Rgba([10, 20, 30, 255]);
        }
        let mut path = std::env::temp_dir();
        path.push("ely_thumb_canvas_test.png");
        src.save(&path).expect("write test png");

        let data = decode_thumb(&path).expect("decode_thumb should succeed");
        // 用实际布局算出的（已拉伸的）卡片宽度，而不是固定 CELL_W。
        let card_w = grid_layout(1680.0).1;
        assert!(card_w >= CELL_W, "卡片应被拉宽而不是压窄");
        let bounds = gpui::Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(card_w), px(IMAGE_H)),
        };
        let image_bounds = ObjectFit::Cover.get_bounds(bounds, data.size(0));
        let visible = bounds.intersect(&image_bounds);
        assert!(
            visible.size.width.as_f32() > 0.0 && visible.size.height.as_f32() > 0.0,
            "绘制区域不能为 0: {:?}",
            visible
        );
        assert!(
            (visible.size.width.as_f32() - card_w).abs() < 0.01,
            "应铺满拉伸后的卡片宽度"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// 回归「最右侧空一列」+「左右边距不等」：任意窗口宽度下，一行的总宽必须
    /// 恰好等于可用宽度、卡片只会比目标宽度更宽，且左右内边距严格相等。
    #[test]
    fn grid_layout_fills_the_row_symmetrically() {
        for w in [640.0, 800.0, 1024.0, 1280.0, 1440.0, 1680.0, 1920.0, 2560.0] {
            let (cols, card_w) = grid_layout(w);
            assert!(cols >= 1, "w={w} 至少要有一列");
            assert!(card_w >= CELL_W - 0.01, "w={w} 卡片被压窄: {card_w}");
            let inner_w = (w - 2.0 * SIDE_PAD).max(CELL_W);
            let used = cols as f32 * card_w + (cols - 1) as f32 * GAP;
            assert!(
                (used - inner_w).abs() < 0.01,
                "w={w} 右侧留白: 占用 {used}, 可用 {inner_w}"
            );
            // 左侧边距 = SIDE_PAD；右侧 = 总宽 - 左 padding - 卡片组宽度。
            let left = SIDE_PAD;
            let right = w - SIDE_PAD - used;
            assert!(
                (left - right).abs() < 0.01,
                "w={w} 左右边距不等: 左 {left}, 右 {right}"
            );
        }
    }

    /// 扫描进度分母与流式扫描的 visited 口径必须一致。
    #[test]
    fn streaming_scan_matches_count_entries() {
        let dir = std::env::temp_dir().join(format!("ely_scan_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("sub"));
        for i in 0..3 {
            std::fs::write(dir.join(format!("a{i}.txt")), b"x").unwrap();
        }
        for i in 0..2 {
            std::fs::write(dir.join("sub").join(format!("b{i}.png")), b"x").unwrap();
        }

        let total = count_entries(&dir);
        let mut visited = 0usize;
        let mut found = 0usize;
        scan_directory_streaming(&dir, &mut |event| {
            match event {
                ScanEvent::Progress { visited: v, .. } => visited = v,
                ScanEvent::Found { .. } => found += 1,
            }
        });
        assert_eq!(visited, total, "visited 必须与总数口径一致");
        assert_eq!(found, 2, "只应发现两张 png");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
