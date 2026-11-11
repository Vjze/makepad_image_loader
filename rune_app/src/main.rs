//! Rune 版图片加载器：一比一复刻 Makepad 版 `img_loader`。
//!
//! 公共逻辑（流式扫描、解码、下采样、LRU 缩略图缓存）全部复用 `image_loader_core`，
//! 只把 UI 与事件循环换成 Rune。

mod actions;
mod scan;
mod tree;
mod ui;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image_loader_core::cache::ThumbnailCache;
use image_loader_core::scanner::{FileTreeData, FileTreeNode};
use image_loader_core::thumbnail::{THUMB_MAX_BITMAPS, THUMB_MAX_IN_FLIGHT};
use rune::{AppSpec, Key, KeyEvent, Pixels, Tx, Waker};

/// 全图查看时的最长边上限，避免把超大原图整张塞进图片图集。
pub const FULL_MAX_DIM: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPhase {
    Idle,
    Counting,
    Scanning,
}

pub struct State {
    // ---- 图片数据与文件树 ----
    pub image_paths: Vec<PathBuf>,
    pub filtered: Vec<usize>,
    pub tree: Option<FileTreeData>,
    pub tree_rows: Vec<tree::TreeRow>,
    pub expanded: HashSet<String>,

    // ---- 后台扫描 ----
    pub scan_msgs: Arc<Mutex<Vec<scan::ScanMsg>>>,
    pub scan_active: bool,
    pub scan_total: usize,
    pub scan_visited: usize,
    pub scan_current: String,
    pub scan_phase: ScanPhase,
    /// 文件夹代际：切换文件夹时递增，旧 worker 的消息与解码结果一律丢弃。
    pub generation: u64,

    // ---- 缩略图 ----
    /// 每个图片索引一个轻量 `Pixels` 句柄；**只有 `thumb_live` 里的索引才持有 RGBA 数据**。
    pub thumb_pixels: HashMap<usize, Pixels>,
    /// 持有 RGBA 数据的图片索引：可见行 ± 缓冲行，加上最近滚出的一批，
    /// 总数由 `ui::THUMB_LIVE_MAX` 限死（128 张 ≈ 22 MB @256）。
    /// 不立即清空刚滚出的卡片，避免滚动时闪空白和重复解码。
    pub thumb_live: HashSet<usize>,
    /// 当前「应当显示」的索引集合（可见行 ± 缓冲行）。`on_visible_rows` 每帧/每次滚动
    /// 更新它；`poll` 用它对「既没缓存、也没在途」的索引补发 `request`，
    /// 抵消 `ThumbnailCache::request` 的 `max_in_flight` 上限把首次请求吞掉的情况。
    pub thumb_keep: HashSet<usize>,
    /// `thumb_live` 的 FIFO 淘汰顺序。
    pub thumb_recent: VecDeque<usize>,
    pub thumb_cache: Arc<ThumbnailCache>,
    /// `ThumbnailCache` 的 ready channel 由 setup 里的桥接线程转到这里。
    pub thumb_ready: Arc<Mutex<Vec<usize>>>,
    pub thumb_rx: Option<Receiver<usize>>,

    // ---- 全图 ----
    pub full_pixels: Pixels,
    pub full_results: Arc<Mutex<Vec<(u64, u64)>>>,
    pub full_token: u64,
    pub full_loading: bool,
    pub img_w: f32,
    pub img_h: f32,

    // ---- 界面状态 ----
    /// 由 `on_layout` 回报的内容区宽度，view 按它算网格列数。
    pub grid_w: f32,

    pub sidebar_open: bool,
    pub slideshow: bool,
    /// 当前图片在 `filtered` 中的位置；`None` 表示从文件树直接打开（不在过滤结果里）。
    pub current: Option<usize>,
    pub direct_idx: Option<usize>,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub drag_last: (f32, f32),

    pub clock: f64,
    pub status: String,
    pub waker: Option<Waker>,
}

fn init() -> State {
    let (thumb_cache, thumb_rx) = ThumbnailCache::new(THUMB_MAX_BITMAPS, THUMB_MAX_IN_FLIGHT);
    State {
        image_paths: Vec::new(),
        filtered: Vec::new(),
        tree: None,
        tree_rows: Vec::new(),
        expanded: HashSet::new(),
        scan_msgs: Arc::new(Mutex::new(Vec::new())),
        scan_active: false,
        scan_total: 0,
        scan_visited: 0,
        scan_current: String::new(),
        scan_phase: ScanPhase::Idle,
        generation: 0,
        thumb_pixels: HashMap::new(),
        thumb_live: HashSet::new(),
        thumb_keep: HashSet::new(),
        thumb_recent: VecDeque::new(),
        thumb_cache,
        thumb_ready: Arc::new(Mutex::new(Vec::new())),
        thumb_rx: Some(thumb_rx),
        full_pixels: Pixels::new(),
        full_results: Arc::new(Mutex::new(Vec::new())),
        full_token: 0,
        full_loading: false,
        img_w: 0.0,
        img_h: 0.0,
        grid_w: 1000.0,
        sidebar_open: false,
        slideshow: false,
        current: None,
        direct_idx: None,
        zoom: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
        drag_last: (0.0, 0.0),
        clock: 0.0,
        status: "未选择文件夹".to_string(),
        waker: None,
    }
}

/// `Waker` 是后台线程唤醒 UI 的唯一通道。缩略图缓存自带的 ready channel 不唤醒窗口，
/// 这里起一个桥接线程把索引搬进 `thumb_ready` 并调用 waker。
fn setup(s: &mut State, wake: Waker) {
    s.waker = Some(wake.clone());
    if let Some(rx) = s.thumb_rx.take() {
        let ready = s.thumb_ready.clone();
        std::thread::spawn(move || {
            for idx in rx {
                ready.lock().unwrap().push(idx);
                wake();
            }
        });
    }
}

fn poll(s: &mut State, t: f64, tx: &mut Tx) -> bool {
    s.clock = t;
    let mut changed = false;

    changed |= scan::drain(s);

    let ready: Vec<usize> = s.thumb_ready.lock().unwrap().drain(..).collect();
    for idx in ready {
        // 从缓存里「取走」而不是克隆：没有第二份常驻 RGBA，也不需要 to_vec 复制。
        let Some((w, h, rgba)) = s.thumb_cache.take_rgba(idx) else {
            continue;
        };
        // 只有仍停留在可见行缓冲内的索引才上传；滚出去的直接丢弃，
        // 等它再次进入视口时重新请求解码。
        if s.thumb_live.contains(&idx)
            && let Some(px) = s.thumb_pixels.get(&idx)
        {
            px.set(w as u32, h as u32, rgba);
        } else {
            // 解码完成时这张图已经滚出缓冲区：退回 LRU，滚回来直接取用，
            // 既不重新解码也不会留下空白卡片。
            s.thumb_cache.put_rgba(idx, w, h, rgba);
        }
    }

    // 重试补发：首次进入可见缓冲时 `ThumbnailCache::request` 受 `max_in_flight` 上限拦截会
    // 直接丢弃请求；而进了 `thumb_live` 的索引不会再触发「entering」，于是永久空白
    // （这就是「缩略图很多不显示」的根因）。这里每帧把「应当显示」却「既没缓存、也没在途」
    // 的索引重新 `request`——`request` 对已缓存/在途的索引幂等返回，对在途槽一空出就补上，
    // 直到 `thumb_keep` 里的缩略图全部解码完成。
    for &idx in &s.thumb_keep {
        if let Some(path) = s.image_paths.get(idx).cloned() {
            s.thumb_cache.request(idx, path);
        }
    }

    let results: Vec<(u64, u64)> = s.full_results.lock().unwrap().drain(..).collect();
    for (token, generation) in results {
        if token == s.full_token && generation == s.generation {
            let (w, h) = s.full_pixels.size();
            s.img_w = w as f32;
            s.img_h = h as f32;
            s.full_loading = false;
            changed = true;
        }
    }

    // 扫描期间让 spinner 保持转动；扫描结束后不再请求额外帧，空闲窗口零绘制。
    if s.scan_active {
        tx.wake_after(Duration::from_millis(50));
        changed = true;
    }
    changed
}

fn on_key(s: &mut State, e: &KeyEvent, _: &mut Tx) -> bool {
    match &e.key {
        Key::Escape if s.slideshow => {
            actions::close_slideshow(s);
            true
        }
        Key::Left if s.slideshow => {
            actions::navigate(s, -1);
            true
        }
        Key::Right if s.slideshow => {
            actions::navigate(s, 1);
            true
        }
        Key::Text(t) if s.slideshow => match t.as_str() {
            "+" | "=" => {
                actions::zoom(s, 1.1);
                true
            }
            "-" | "_" => {
                actions::zoom(s, 1.0 / 1.1);
                true
            }
            "0" => {
                actions::reset_zoom(s);
                true
            }
            _ => false,
        },
        Key::Enter if !s.slideshow && !s.filtered.is_empty() => {
            actions::open_slideshow(s);
            true
        }
        _ => false,
    }
}

/// 造一张渐变假图，供 `--scene` 快照使用（不读磁盘，headless 也能画出内容）。
fn demo_pixels(seed: usize, w: u32, h: u32) -> Pixels {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 / w as f32 + y as f32 / h as f32) * 0.5;
            let r = (40.0 + 100.0 * t + (seed as f32 * 37.0) % 90.0) as u8;
            let g = (60.0 + 90.0 * (1.0 - t) + (seed as f32 * 71.0) % 60.0) as u8;
            let b = (110.0 + 100.0 * t) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    let p = Pixels::new();
    p.set(w, h, rgba);
    p
}

fn demo_grid(s: &mut State) {
    for i in 0..12 {
        s.image_paths
            .push(PathBuf::from(format!("demo/pic_{i:02}.png")));
        s.filtered.push(i);
        s.thumb_pixels.insert(i, demo_pixels(i, 64, 48));
        s.thumb_live.insert(i);
        s.thumb_recent.push_back(i);
    }
    let children: Vec<FileTreeNode> = (0..12)
        .map(|i| FileTreeNode {
            path: format!("demo/pic_{i:02}.png"),
            name: format!("pic_{i:02}.png"),
            image_idx: Some(i),
            children: Vec::new(),
        })
        .collect();
    s.tree = Some(FileTreeData {
        dir_path: PathBuf::from("demo"),
        root: FileTreeNode {
            path: "demo".to_string(),
            name: "demo".to_string(),
            image_idx: None,
            children,
        },
    });
    s.sidebar_open = true;
    tree::rebuild(s);
    s.current = Some(0);
    s.status = "共: 12张".to_string();
}

fn demo_slideshow(s: &mut State) {
    demo_grid(s);
    s.full_pixels = demo_pixels(3, 320, 200);
    s.img_w = 320.0;
    s.img_h = 200.0;
    s.slideshow = true;
}

/// 扫描中间态：进度条 + 边扫边写已经出现的卡片。
fn demo_scanning(s: &mut State) {
    for i in 0..6 {
        s.image_paths
            .push(PathBuf::from(format!("demo/scanning_{i:02}.png")));
        s.filtered.push(i);
        s.thumb_pixels.insert(i, demo_pixels(i + 20, 64, 48));
        s.thumb_live.insert(i);
        s.thumb_recent.push_back(i);
    }
    s.scan_active = true;
    s.scan_total = 100;
    s.scan_visited = 42;
    s.scan_current = "scanning_042.png".to_string();
    s.scan_phase = ScanPhase::Scanning;
    s.status = "已发现: 6张".to_string();
    s.clock = 0.3;
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[
            ("empty", |_| {}),
            ("grid", demo_grid),
            ("scanning", demo_scanning),
            ("slideshow", demo_slideshow),
        ],
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("Image_Loader", (1280.0, 768.0), init, ui::view)
    });
}
