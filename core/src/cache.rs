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

use crate::decode::decode_rgba8_scaled;
use crate::thumbnail::THUMB_MAX_DIM;

/// 一张已解码的缩略图（RGBA8）。
#[derive(Clone)]
pub struct ThumbBitmap {
    pub width: usize,
    pub height: usize,
    /// 用 `Arc<Vec<u8>>` 而不是 `Arc<[u8]>`：后者是 unsized，无法 `try_unwrap`
    /// 零拷贝取回缓冲区。deref 到 `&[u8]` 的调用方写法不变。
    pub rgba: Arc<Vec<u8>>,
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

    /// 取出并**移除**已解码位图，尽量零拷贝地把像素转成 `Vec<u8>`。
    ///
    /// 给 Rune 这类「自己维护可见行像素」的 UI 用：它把数据交给 `Pixels` 后，
    /// 缓存里不需要再常驻第二份 RGBA。没有其他人持有这份像素时
    /// `Arc::try_unwrap` 命中，`Box<[u8]> -> Vec<u8>` 是零拷贝。
    pub fn take_rgba(&self, index: usize) -> Option<(usize, usize, Vec<u8>)> {
        let bitmap = self.bitmaps.lock().unwrap().remove(&index)?;
        // order 里可能还留着这个索引，顺手清掉，避免反复请求时队列无限增长。
        self.order.lock().unwrap().retain(|i| *i != index);
        let rgba = match Arc::try_unwrap(bitmap.rgba) {
            Ok(owned) => owned,
            Err(shared) => shared.as_ref().clone(),
        };
        Some((bitmap.width, bitmap.height, rgba))
    }

    /// 把一份已解码位图放回 LRU（不触发 ready 通知，避免唤醒循环）。
    ///
    /// UI 收到 ready 时若该图已滚出视口，就把数据退回来而不是丢掉：
    /// 滚回时可以直接取用，不必重新解码，也不会出现空白卡片。
    pub fn put_rgba(&self, index: usize, width: usize, height: usize, rgba: Vec<u8>) {
        let bitmap = ThumbBitmap {
            width,
            height,
            rgba: Arc::new(rgba),
        };
        let mut bm = self.bitmaps.lock().unwrap();
        let mut order = self.order.lock().unwrap();
        bm.insert(index, bitmap);
        order.retain(|i| *i != index);
        order.push_back(index);
        while bm.len() > self.max_bitmaps {
            if let Some(victim) = order.pop_front() {
                bm.remove(&victim);
            } else {
                break;
            }
        }
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
            // 解码时就缩到 THUMB_MAX_DIM，不再先物化全尺寸 RGBA8 再下采样。
            let result = decode_rgba8_scaled(&path, THUMB_MAX_DIM as u32).ok();
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
                width: w as usize,
                height: h as usize,
                rgba: Arc::new(rgba),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(cache: &ThumbnailCache, index: usize, rgba: Vec<u8>) -> *const u8 {
        let bitmap = ThumbBitmap {
            width: 2,
            height: 1,
            rgba: Arc::new(rgba),
        };
        let ptr = bitmap.rgba.as_ptr();
        cache.bitmaps.lock().unwrap().insert(index, bitmap);
        cache.order.lock().unwrap().push_back(index);
        ptr
    }

    #[test]
    fn take_rgba_moves_the_buffer_without_copying_when_unshared() {
        let (cache, _rx) = ThumbnailCache::new(8, 2);
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let ptr = seed(&cache, 7, data.clone());

        let (w, h, got) = cache.take_rgba(7).expect("bitmap should be delivered");
        assert_eq!((w, h), (2, 1));
        assert_eq!(got, data);
        assert_eq!(got.as_ptr(), ptr, "unshared bitmap must be moved, not copied");
        assert!(cache.take_rgba(7).is_none(), "delivery removes the cached copy");
        assert!(cache.order.lock().unwrap().is_empty(), "LRU order is cleaned");
    }

    #[test]
    fn take_rgba_falls_back_to_a_copy_when_the_bitmap_is_shared() {
        let (cache, _rx) = ThumbnailCache::new(8, 2);
        let ptr = seed(&cache, 3, vec![9u8; 8]);
        let shared = cache.get(3).expect("seeded");
        assert!(Arc::strong_count(&shared.rgba) > 1);

        let (w, h, got) = cache.take_rgba(3).expect("bitmap should be delivered");
        assert_eq!((w, h), (2, 1));
        assert_eq!(got, vec![9u8; 8]);
        assert_ne!(got.as_ptr(), ptr, "shared bitmap has to be cloned");
    }
}
