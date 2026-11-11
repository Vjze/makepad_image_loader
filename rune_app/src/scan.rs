//! 后台文件夹扫描：两阶段（先统计总数，再流式扫描），按批次唤醒 UI 边扫边写。
//!
//! 与 Makepad 版 `src/scan_worker.rs` 一一对应，只是把 `SignalToUI::set_ui_signal()`
//! 换成 Rune 的 `Waker`，消息经共享队列在 `poll` 里消费。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image_loader_core::scanner::{
    FileTreeData, ScanEvent, count_entries, scan_directory_streaming,
};
use rune::{Pixels, Waker};

use crate::{ScanPhase, State, actions, tree};

/// 每访问这么多个目录项强制推一批，避免大目录刷屏式唤醒 UI。
const FLUSH_ENTRIES: usize = 64;
/// 最长多久推一批，保证小目录也能看到进度在走。
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug)]
pub enum ScanMsg {
    Started { generation: u64 },
    Counted { generation: u64, total: usize },
    Batch {
        generation: u64,
        visited: usize,
        current: String,
        found: Vec<(usize, PathBuf)>,
    },
    Done {
        generation: u64,
        visited: usize,
        tree: FileTreeData,
    },
}

impl ScanMsg {
    pub fn generation(&self) -> u64 {
        match self {
            ScanMsg::Started { generation }
            | ScanMsg::Counted { generation, .. }
            | ScanMsg::Batch { generation, .. }
            | ScanMsg::Done { generation, .. } => *generation,
        }
    }
}

fn push(queue: &Arc<Mutex<Vec<ScanMsg>>>, wake: &Waker, msg: ScanMsg) {
    queue.lock().unwrap().push(msg);
    wake();
}

/// 开始扫描新文件夹：清空旧数据、递增代际、丢弃旧缩略图，然后起 worker 线程。
pub fn start(s: &mut State, path: PathBuf) {
    let Some(wake) = s.waker.clone() else {
        return;
    };

    s.generation = s.generation.wrapping_add(1);
    let generation = s.generation;

    s.image_paths.clear();
    s.filtered.clear();
    s.thumb_pixels.clear();
    s.thumb_live.clear();
    s.thumb_recent.clear();
    s.tree = None;
    s.tree_rows.clear();
    s.expanded.clear();
    s.current = None;
    s.direct_idx = None;
    s.slideshow = false;
    s.zoom = 1.0;
    s.pan_x = 0.0;
    s.pan_y = 0.0;
    s.img_w = 0.0;
    s.img_h = 0.0;
    s.full_token = s.full_token.wrapping_add(1);
    s.full_loading = false;
    s.full_results.lock().unwrap().clear();
    s.sidebar_open = false;

    s.scan_active = true;
    s.scan_total = 0;
    s.scan_visited = 0;
    s.scan_current.clear();
    s.scan_phase = ScanPhase::Counting;
    s.scan_msgs.lock().unwrap().clear();
    s.status = "正在统计文件数…".to_string();

    s.thumb_cache.clear();
    s.thumb_cache.bump_generation();
    s.thumb_ready.lock().unwrap().clear();

    let queue = s.scan_msgs.clone();
    std::thread::spawn(move || run(path, generation, queue, wake));
}

fn run(path: PathBuf, generation: u64, queue: Arc<Mutex<Vec<ScanMsg>>>, wake: Waker) {
    push(
        &queue,
        &wake,
        ScanMsg::Started { generation },
    );

    // 阶段一：只列目录统计总数，作为进度条分母。
    let total = count_entries(&path);
    push(
        &queue,
        &wake,
        ScanMsg::Counted { generation, total },
    );

    // 阶段二：流式扫描，边发现边把图片推给 UI。
    let mut visited = 0usize;
    let mut current = String::new();
    let mut pending: Vec<(usize, PathBuf)> = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_flushed_visited = 0usize;

    let root = scan_directory_streaming(&path, &mut |event| {
        match event {
            ScanEvent::Progress { visited: v, current: name } => {
                visited = v;
                current = name;
            }
            ScanEvent::Found { index, path } => pending.push((index, path)),
        }
        let due = visited - last_flushed_visited >= FLUSH_ENTRIES
            || last_flush.elapsed() >= FLUSH_INTERVAL;
        if due {
            push(
                &queue,
                &wake,
                ScanMsg::Batch {
                    generation,
                    visited,
                    current: current.clone(),
                    found: std::mem::take(&mut pending),
                },
            );
            last_flush = Instant::now();
            last_flushed_visited = visited;
        }
    });

    if !pending.is_empty() {
        push(
            &queue,
            &wake,
            ScanMsg::Batch {
                generation,
                visited,
                current: current.clone(),
                found: pending,
            },
        );
    }

    push(
        &queue,
        &wake,
        ScanMsg::Done {
            generation,
            visited,
            tree: FileTreeData {
                dir_path: path,
                root,
            },
        },
    );
}

/// 在 `poll` 里排空队列；切换文件夹后旧 worker 的消息按代际丢弃。
pub fn drain(s: &mut State) -> bool {
    let msgs: Vec<ScanMsg> = s.scan_msgs.lock().unwrap().drain(..).collect();
    let mut changed = false;
    for msg in msgs {
        if msg.generation() != s.generation {
            continue;
        }
        changed |= apply(s, msg);
    }
    changed
}

/// 在 `poll` 里消费一条增量消息；返回是否改变了 view 读取的状态。
pub fn apply(s: &mut State, msg: ScanMsg) -> bool {
    match msg {
        ScanMsg::Started { .. } => {
            s.scan_phase = ScanPhase::Counting;
            s.status = "正在统计文件数…".to_string();
            true
        }
        ScanMsg::Counted { total, .. } => {
            s.scan_total = total;
            s.scan_phase = ScanPhase::Scanning;
            s.status = format!("扫描中 0/{total}");
            true
        }
        ScanMsg::Batch {
            visited,
            current,
            found,
            ..
        } => {
            s.scan_visited = visited;
            s.scan_current = current;
            s.scan_phase = ScanPhase::Scanning;
            // scanner 生成的 index 严格连续，可直接追加；句柄很轻，只在进入
            // 可见行缓冲时才由 `on_visible_rows` 决定谁真正持有像素数据。
            for (idx, path) in found {
                if idx == s.image_paths.len() {
                    s.image_paths.push(path);
                    s.filtered.push(idx);
                    s.thumb_pixels.insert(idx, Pixels::new());
                }
            }
            s.status = format!("已发现: {}张", s.image_paths.len());
            true
        }
        ScanMsg::Done { visited, tree: data, .. } => {
            s.scan_visited = visited;
            s.scan_active = false;
            s.scan_phase = ScanPhase::Idle;
            s.tree = Some(data);
            s.sidebar_open = true;
            s.expanded.clear();
            tree::rebuild(s);
            s.status = format!("共: {}张", s.image_paths.len());
            actions::apply_filter(s, "");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image_loader_core::scanner::FileTreeNode;

    fn node() -> FileTreeNode {
        FileTreeNode {
            path: "demo".into(),
            name: "demo".into(),
            image_idx: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn batches_append_in_index_order_and_done_opens_the_sidebar() {
        let mut s = crate::init();
        // 真实流程里 scan::start 会置位；测试不启动线程，直接模拟扫描进行中。
        s.scan_active = true;
        assert!(apply(
            &mut s,
            ScanMsg::Counted {
                generation: 0,
                total: 10
            }
        ));
        assert_eq!(s.scan_total, 10);
        assert!(apply(
            &mut s,
            ScanMsg::Batch {
                generation: 0,
                visited: 5,
                current: "b.png".into(),
                found: vec![
                    (0, PathBuf::from("a.png")),
                    (1, PathBuf::from("b.png")),
                ],
            }
        ));
        assert_eq!(s.image_paths.len(), 2);
        assert_eq!(s.filtered, vec![0, 1]);
        assert_eq!(s.thumb_pixels.len(), 2);
        assert_eq!(s.scan_visited, 5);
        assert!(s.scan_active);

        assert!(apply(
            &mut s,
            ScanMsg::Done {
                generation: 0,
                visited: 10,
                tree: FileTreeData {
                    dir_path: PathBuf::from("demo"),
                    root: node(),
                },
            }
        ));
        assert!(!s.scan_active);
        assert!(s.sidebar_open);
        assert_eq!(s.status, "共: 2张");
        assert_eq!(s.current, Some(0));
    }

    #[test]
    fn a_stale_generation_is_dropped_before_it_reaches_the_ui() {
        let mut s = crate::init();
        let stale = s.generation + 1;
        s.scan_msgs.lock().unwrap().push(ScanMsg::Batch {
            generation: stale,
            visited: 1,
            current: "old.png".into(),
            found: vec![(0, PathBuf::from("old.png"))],
        });
        assert!(!drain(&mut s));
        assert!(s.image_paths.is_empty());

        s.scan_msgs.lock().unwrap().push(ScanMsg::Batch {
            generation: s.generation,
            visited: 1,
            current: "new.png".into(),
            found: vec![(0, PathBuf::from("new.png"))],
        });
        assert!(drain(&mut s));
        assert_eq!(s.image_paths, vec![PathBuf::from("new.png")]);
    }
}
