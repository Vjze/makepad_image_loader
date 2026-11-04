//! 后台文件夹扫描：worker 线程 + `SignalToUI` 唤醒 UI 线程。
//!
//! Makepad 的事件循环在收到 `Event::Signal` 时会把控制权交回应用；worker 把
//! 增量结果放进 mpsc 队列后调用 `SignalToUI::set_ui_signal()` 把 UI 线程叫醒，
//! App 在 `Event::Signal` 里一次性 `try_recv` 完所有批次，边扫边把图片追加进
//! 缩略图列表，并更新进度条。整个过程不阻塞 UI 线程。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use image_loader_core::scanner::{
    count_entries, scan_directory_streaming, FileTreeData, FileTreeNode, ScanEvent,
};
use makepad_widgets::makepad_platform::thread::SignalToUI;

/// 每访问这么多个目录项强制推一批，避免大目录刷屏式唤醒 UI。
const FLUSH_ENTRIES: usize = 64;
/// 最长多久推一批，保证小目录也能看到进度在走。
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);

/// worker 发给 UI 的增量消息。全部携带代际，切换文件夹后旧消息会被丢弃。
#[derive(Debug)]
pub enum ScanMsg {
    /// 线程已启动，正在统计目录项总数（进度条分母）。
    Started { generation: u64 },
    /// 统计完成，`total` 即 `count_entries` 的结果。
    Counted { generation: u64, total: usize },
    /// 一批增量：`found` 是本批新发现的图片（index 与 image_paths 下标一致）。
    Batch {
        generation: u64,
        visited: usize,
        current: String,
        found: Vec<(usize, PathBuf)>,
    },
    /// 扫描完成，携带完整目录树。
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

/// 挂在 `App` 上的扫描句柄。它没有后台线程所有权，只持有接收端和聚合后的进度。
#[derive(Default)]
pub struct ScanWorker {
    rx: Option<Receiver<ScanMsg>>,
    generation: u64,
    total: usize,
    visited: usize,
    current: String,
    active: bool,
}

impl ScanWorker {
    /// 启动一次新扫描。旧接收端被替换，旧线程的消息随之无人接收（自然丢弃）。
    pub fn start(&mut self, path: PathBuf, generation: u64) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.generation = generation;
        self.total = 0;
        self.visited = 0;
        self.current = String::new();
        self.active = true;
        std::thread::spawn(move || run_scan(path, generation, tx));
    }

    /// 在 `Event::Signal` 中调用：取走队列里所有消息，并聚合进度字段。
    /// `Batch.found` 不在这里聚合，由 App 逐条追加到图片列表。
    pub fn poll(&mut self) -> Vec<ScanMsg> {
        let Some(rx) = self.rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(_) => break,
            }
        }
        for msg in &out {
            match msg {
                ScanMsg::Counted { total, .. } => self.total = *total,
                ScanMsg::Batch {
                    visited, current, ..
                } => {
                    self.visited = *visited;
                    self.current = current.clone();
                }
                ScanMsg::Done { visited, .. } => {
                    self.visited = *visited;
                    self.active = false;
                }
                ScanMsg::Started { .. } => {}
            }
        }
        out
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn visited(&self) -> usize {
        self.visited
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 0.0..=1.0；统计阶段（分母未知）返回 0。
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.visited as f32 / self.total as f32).min(1.0)
    }
}

fn run_scan(path: PathBuf, generation: u64, tx: Sender<ScanMsg>) {
    let _ = tx.send(ScanMsg::Started { generation });
    SignalToUI::set_ui_signal();

    // 阶段一：只列目录统计总数，作为进度条分母。
    let total = count_entries(&path);
    let _ = tx.send(ScanMsg::Counted { generation, total });
    SignalToUI::set_ui_signal();

    // 阶段二：流式扫描，边发现边把图片推给 UI。
    let mut visited = 0usize;
    let mut current = String::new();
    let mut pending: Vec<(usize, PathBuf)> = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_flushed_visited = 0usize;

    let root: FileTreeNode = scan_directory_streaming(&path, &mut |event| {
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
            let found = std::mem::take(&mut pending);
            let _ = tx.send(ScanMsg::Batch {
                generation,
                visited,
                current: current.clone(),
                found,
            });
            SignalToUI::set_ui_signal();
            last_flush = Instant::now();
            last_flushed_visited = visited;
        }
    });

    if !pending.is_empty() {
        let _ = tx.send(ScanMsg::Batch {
            generation,
            visited,
            current: current.clone(),
            found: pending,
        });
        SignalToUI::set_ui_signal();
    }

    let _ = tx.send(ScanMsg::Done {
        generation,
        visited,
        tree: FileTreeData {
            dir_path: path,
            root,
        },
    });
    SignalToUI::set_ui_signal();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "mp_scan_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn count_image_nodes(node: &FileTreeNode) -> usize {
        node.children
            .iter()
            .map(|c| usize::from(c.image_idx.is_some()) + count_image_nodes(c))
            .sum()
    }

    /// 模拟 UI 侧「边扫边写」：批次里的 index 必须严格连续，否则 App 无法
    /// 直接 push 进 image_paths / filtered_image_idxs。
    #[test]
    fn streaming_scan_emits_ordered_batches_and_matches_total() {
        let root = temp_dir("ordered");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b/c")).unwrap();
        for i in 0..12 {
            let dir = match i % 3 {
                0 => root.join("a"),
                1 => root.join("b/c"),
                _ => root.clone(),
            };
            fs::write(dir.join(format!("pic_{:02}.png", i)), b"not a real png").unwrap();
        }
        fs::write(root.join("notes.txt"), b"ignore").unwrap();

        let (tx, rx) = channel();
        run_scan(root.clone(), 7, tx);
        let msgs: Vec<ScanMsg> = rx.try_iter().collect();

        assert!(matches!(
            msgs.first(),
            Some(ScanMsg::Started { generation: 7 })
        ));
        let total = match &msgs[1] {
            ScanMsg::Counted {
                generation: 7,
                total,
            } => *total,
            other => panic!("expected Counted, got {:?}", other),
        };
        assert_eq!(total, count_entries(&root));
        assert!(msgs.iter().all(|m| m.generation() == 7));

        let mut found: Vec<(usize, PathBuf)> = Vec::new();
        let mut last_visited = 0;
        for msg in &msgs {
            if let ScanMsg::Batch {
                visited,
                found: batch,
                ..
            } = msg
            {
                assert!(*visited >= last_visited, "visited must be monotonic");
                last_visited = *visited;
                found.extend(batch.iter().cloned());
            }
        }
        assert_eq!(found.len(), 12);
        for (i, (idx, _)) in found.iter().enumerate() {
            assert_eq!(*idx, i, "index must stay contiguous for incremental append");
        }

        match msgs.last().expect("Done") {
            ScanMsg::Done { visited, tree, .. } => {
                assert_eq!(*visited, total, "进度条分子最终应追上分母");
                assert_eq!(count_image_nodes(&tree.root), 12);
                assert_eq!(tree.dir_path, root);
            }
            other => panic!("expected Done, got {:?}", other),
        }

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn worker_poll_aggregates_progress() {
        let (tx, rx) = channel();
        let mut worker = ScanWorker::default();
        worker.rx = Some(rx);
        tx.send(ScanMsg::Counted {
            generation: 3,
            total: 100,
        })
        .unwrap();
        tx.send(ScanMsg::Batch {
            generation: 3,
            visited: 50,
            current: "x.png".to_string(),
            found: Vec::new(),
        })
        .unwrap();
        let polled = worker.poll();
        assert_eq!(polled.len(), 2);
        assert_eq!(worker.visited(), 50);
        assert!((worker.progress() - 0.5).abs() < 1e-6);
        assert!(worker.poll().is_empty(), "队列应被一次取空");
    }
}
