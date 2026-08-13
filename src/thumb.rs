use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};

// 缩略图后台解码参数
pub(crate) const THUMB_MAX_DIM: usize = 512;
pub(crate) const THUMB_MAX_IN_FLIGHT: usize = 8;
pub(crate) const THUMB_MAX_TEXTURES: usize = 96;

// ZoomableView 与 App 共用的缩放状态 statics（draw_walk 时读取）
pub(crate) static ZOOM_FACTOR: AtomicU64 = AtomicU64::new(f64::to_bits(1.0));
pub(crate) static ZOOM_PAN_X: AtomicU64 = AtomicU64::new(f64::to_bits(0.0));
pub(crate) static ZOOM_PAN_Y: AtomicU64 = AtomicU64::new(f64::to_bits(0.0));

// 把缩放/平移状态写入 makepad 的全局 statics
pub(crate) fn store_zoom_state(zoom: f64, pan: Vec2d) {
    ZOOM_FACTOR.store(f64::to_bits(zoom), Ordering::Relaxed);
    ZOOM_PAN_X.store(f64::to_bits(pan.x), Ordering::Relaxed);
    ZOOM_PAN_Y.store(f64::to_bits(pan.y), Ordering::Relaxed);
}

// 把图片 buffer 缩小到最长边不超过 max_dim（用于缩略图，避免全尺寸贴图占显存）。
// 采用盒式平均下采样。
pub(crate) fn downscale_image(
    buf: &ImageBuffer,
    max_dim: usize,
) -> Option<(usize, usize, Vec<u32>)> {
    let (w, h) = (buf.width, buf.height);
    if w == 0 || h == 0 {
        return None;
    }
    if w <= max_dim && h <= max_dim {
        return Some((w, h, buf.data.clone()));
    }
    let scale = max_dim as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round().max(1.0)) as usize;
    let nh = ((h as f64 * scale).round().max(1.0)) as usize;
    let mut out = Vec::with_capacity(nw * nh);
    for oy in 0..nh {
        let sy0 = (oy as f64 * h as f64 / nh as f64).floor() as usize;
        let sy1 = (((oy + 1) as f64 * h as f64 / nh as f64).ceil() as usize)
            .max(sy0 + 1)
            .min(h);
        for ox in 0..nw {
            let sx0 = (ox as f64 * w as f64 / nw as f64).floor() as usize;
            let sx1 = (((ox + 1) as f64 * w as f64 / nw as f64).ceil() as usize)
                .max(sx0 + 1)
                .min(w);
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for y in sy0..sy1 {
                let row = y * w;
                for x in sx0..sx1 {
                    let p = buf.data[row + x];
                    a += ((p >> 24) & 0xFF) as u64;
                    r += ((p >> 16) & 0xFF) as u64;
                    g += ((p >> 8) & 0xFF) as u64;
                    b += (p & 0xFF) as u64;
                }
            }
            let n = ((sy1 - sy0) * (sx1 - sx0)) as u64;
            if let (Some(av), Some(rv), Some(gv), Some(bv)) = (
                a.checked_div(n),
                r.checked_div(n),
                g.checked_div(n),
                b.checked_div(n),
            ) {
                out.push(((av as u32) << 24) | ((rv as u32) << 16) | ((gv as u32) << 8) | (bv as u32));
            }
        }
    }
    Some((nw, nh, out))
}
