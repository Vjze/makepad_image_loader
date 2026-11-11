//! 缩略图下采样与像素格式工具（框架无关）。
//!
//! - `THUMB_*` 常量：下采样上限、在途解码上限、缓存位图上限（LRU）。
//! - `downscale_rgba8`：盒式平均下采样，把最长边压到 `max_dim` 以内，控制显存/内存。
//! - `rgba8_to_argb_u32`：把 RGBA8 转成 Makepad 纹理期望的 `0xAARRGGBB` 像素序。

/// 缩略图最长边上限（像素）。
///
/// 卡片显示区域约 240×266，256 已足够（含高 DPI 下 2 倍以内仍可接受）。
/// 这个值同时决定解码后的 RGBA8 常驻副本与 Rune 图集 slot 的尺寸，
/// 从 512 降到 256 可让两者各降约 4 倍，是最省事的内存优化。
pub const THUMB_MAX_DIM: usize = 256;
/// 同时在途的解码任务上限（防止一次性起太多线程）。
pub const THUMB_MAX_IN_FLIGHT: usize = 8;
/// 缓存的已解码位图数量上限（LRU 淘汰）。
pub const THUMB_MAX_BITMAPS: usize = 96;

/// 盒式平均下采样：把 RGBA8 缓冲的最长边压到 `max_dim` 以内。
///
/// 当原图已小于 `max_dim` 时直接返回拷贝，不做额外工作。
pub fn downscale_rgba8(
    data: &[u8],
    w: usize,
    h: usize,
    max_dim: usize,
) -> Option<(usize, usize, Vec<u8>)> {
    if w == 0 || h == 0 || data.len() < w * h * 4 {
        return None;
    }
    if w <= max_dim && h <= max_dim {
        return Some((w, h, data.to_vec()));
    }
    let scale = max_dim as f64 / (w.max(h)) as f64;
    let nw = ((w as f64 * scale).round().max(1.0)) as usize;
    let nh = ((h as f64 * scale).round().max(1.0)) as usize;
    let mut out = Vec::with_capacity(nw * nh * 4);
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
            let mut r = 0u64;
            let mut g = 0u64;
            let mut b = 0u64;
            let mut a = 0u64;
            let mut cnt = 0u64;
            for y in sy0..sy1 {
                let base = y * w * 4;
                for x in sx0..sx1 {
                    let p = base + x * 4;
                    r += data[p] as u64;
                    g += data[p + 1] as u64;
                    b += data[p + 2] as u64;
                    a += data[p + 3] as u64;
                    cnt += 1;
                }
            }
            let inv = 1.0 / cnt as f64;
            out.push((r as f64 * inv) as u8);
            out.push((g as f64 * inv) as u8);
            out.push((b as f64 * inv) as u8);
            out.push((a as f64 * inv) as u8);
        }
    }
    Some((nw, nh, out))
}

/// 把 RGBA8 像素（`[r,g,b,a]` 顺序）转换为 Makepad 纹理期望的 `0xAARRGGBB`（`u32`）像素序。
///
/// Makepad 的 `ImageBuffer.data` 为 `Vec<u32>`，高位为 alpha、随后 R/G/B，与其自带解码器一致。
pub fn rgba8_to_argb_u32(rgba: &[u8]) -> Vec<u32> {
    rgba
        .chunks_exact(4)
        .map(|p| {
            ((p[3] as u32) << 24)
                | ((p[0] as u32) << 16)
                | ((p[1] as u32) << 8)
                | (p[2] as u32)
        })
        .collect()
}
