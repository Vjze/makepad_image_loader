//! 共享缩略图缓存（框架无关）。
//!
//! 负责：后台线程解码 + 盒式下采样、在途任务上限、LRU 淘汰、文件夹代际（generation）丢弃。
//! 缓存只保存 **解码后的 RGBA8 位图**，不触及任何 GPU 资源；各 UI 自行把位图上传到
//! 自己的纹理 / `ImageData`。Makepad 与 Xilem 可共用同一套调度逻辑。
//!
//! 典型用法：
//! - `ThumbnailCache::new(...)` 拿到 `(cache, ready_rx)`，`ready_rx` 可选（Xilem 用一个后台
//!   worker 接收并触发重绘；Makepad 在 `draw_walk` 里轮询 `get` 即可）。
//! - 切换文件夹时 `cache.clear()` + `cache.bump_generation()`。
//! - 渲染某卡片时：`if cache.contains(idx) { 上传 } else if !cache.is_inflight(idx) { cache.request(idx, path) }`。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::decode::decode_rgba8;
use crate::thumbnail::{downscale_rgba8, THUMB_MAX_DIM};

/// 一张已解码的缩略图（RGBA8）。
#[derive(Clone)]
pub struct ThumbBitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<[u8]>,
}

/// 共享缩略图缓存。
pub struct ThumbnailCache {
    max_bitmaps: usize,
    max_in_flight: usize,
    bitmaps: Mutex<HashMap<usize, ThumbBitmap>>,
    order: Mutex<VecDeque<usize>>,
    in_flight: Mutex<HashSet<usize>>,
    generation: Mutex<u64>,
    notify: Mutex<Option<Sender<usize>>>,
}

impl ThumbnailCache {
    /// 创建缓存，返回 `(cache, ready_rx)`。
    ///
    /// `ready_rx` 可选：当某张缩略图解码完成时会通过它发出其图片索引，供后台 worker 触发 UI 重绘。
    pub fn new(max_bitmaps: usize, max_in_flight: usize) -> (Arc<Self>, Receiver<usize>) {
        let (tx, rx) = channel();
        let cache = Arc::new(Self {
            max_bitmaps,
            max_in_flight,
            bitmaps: Mutex::new(HashMap::new()),
            order: Mutex::new(VecDeque::new()),
            in_flight: Mutex::new(HashSet::new()),
            generation: Mutex::new(0),
            notify: Mutex::new(Some(tx)),
        });
        (cache, rx)
    }

    /// 当前代际。
    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    /// 切换文件夹时调用：递增代际，使旧文件夹遗留的后台结果在回传时被丢弃。
    pub fn bump_generation(&self) -> u64 {
        let mut g = self.generation.lock().unwrap();
        *g = g.wrapping_add(1);
        *g
    }

    pub fn set_generation(&self, g: u64) {
        *self.generation.lock().unwrap() = g;
    }

    /// 清空所有已解码位图与在途任务（切换文件夹时调用）。
    pub fn clear(&self) {
        self.bitmaps.lock().unwrap().clear();
        self.order.lock().unwrap().clear();
        self.in_flight.lock().unwrap().clear();
    }

    /// 是否已有该索引的位图。
    pub fn contains(&self, index: usize) -> bool {
        self.bitmaps.lock().unwrap().contains_key(&index)
    }

    /// 该索引是否正在解码中。
    pub fn is_inflight(&self, index: usize) -> bool {
        self.in_flight.lock().unwrap().contains(&index)
    }

    /// 取出已解码位图（克隆，内部引用计数，开销很小）。
    pub fn get(&self, index: usize) -> Option<ThumbBitmap> {
        self.bitmaps.lock().unwrap().get(&index).cloned()
    }

    /// 请求解码某索引。若该索引已存在、在途或达在途上限，则忽略。
    ///
    /// 解码在后台线程完成，成功后写入缓存（LRU 淘汰）并通过 `ready_rx` 通知。
    pub fn request(self: &Arc<Self>, index: usize, path: PathBuf) {
        let mut inflight = self.in_flight.lock().unwrap();
        if self.bitmaps.lock().unwrap().contains_key(&index) || inflight.contains(&index) {
            return;
        }
        if inflight.len() >= self.max_in_flight {
            return;
        }
        inflight.insert(index);
        let cache: Arc<Self> = Arc::clone(self);
        let generation = *self.generation.lock().unwrap();
        thread::spawn(move || {
            let result = decode_rgba8(&path)
                .ok()
                .and_then(|(w, h, d)| downscale_rgba8(&d, w as usize, h as usize, THUMB_MAX_DIM));
            // 先移除在途标记，再决定是否落库（顺序无关紧要，锁很短）。
            cache.in_flight.lock().unwrap().remove(&index);
            let Some((w, h, rgba)) = result else {
                return;
            };
            // 丢弃过期代际的结果（来自上一个文件夹的后台线程）。
            if *cache.generation.lock().unwrap() != generation {
                return;
            }
            let bitmap = ThumbBitmap {
                width: w,
                height: h,
                rgba: Arc::from(rgba),
            };
            let mut bm = cache.bitmaps.lock().unwrap();
            let mut order = cache.order.lock().unwrap();
            bm.insert(index, bitmap);
            order.push_back(index);
            while bm.len() > cache.max_bitmaps {
                if let Some(victim) = order.pop_front() {
                    bm.remove(&victim);
                } else {
                    break;
                }
            }
            // 通知 UI 有新缩略图可用。
            if let Some(tx) = cache.notify.lock().unwrap().as_ref() {
                let _ = tx.send(index);
            }
        });
    }
}
