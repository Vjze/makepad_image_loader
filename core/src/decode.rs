//! 图像解码（框架无关）。
//!
//! 原来 Makepad 版在 `image_grid.rs` 里用 makepad 自己的 `decode_image_from_data` 解码，
//! Xilem 版则需要 RGBA8 像素再交给 peniko。这里统一用 `image` crate 解码为 RGBA8，
//! 两个 UI 都是同样的解码路径，只是最后上传到各自 GPU 纹理/ImageData 的方式不同。

use std::path::Path;

/// 解码错误。
#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    Image(image::ImageError),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "IO error: {e}"),
            DecodeError::Image(e) => write!(f, "image decode error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e)
    }
}

impl From<image::ImageError> for DecodeError {
    fn from(e: image::ImageError) -> Self {
        DecodeError::Image(e)
    }
}

/// 解码整个文件为 `image::DynamicImage`（保留原始色彩/方向，供全图查看使用）。
pub fn decode_image(path: &Path) -> Result<image::DynamicImage, DecodeError> {
    Ok(image::open(path)?)
}

/// 解码文件为 RGBA8 像素：`(width, height, rgba_bytes)`。
///
/// `rgba_bytes` 长度恒为 `width * height * 4`，顺序为 `[r, g, b, a]`。
/// 注意：这个函数会物化**全尺寸** RGBA8，只适合确实需要原始分辨率的场合；
/// 缩略图/全图预览请用 [`decode_rgba8_scaled`]。
pub fn decode_rgba8(path: &Path) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    let img = decode_image(path)?;
    // 先转成 RGBA8 再取数据，统一像素布局，避免各后端格式差异。
    let rgba = img.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((w, h, rgba.into_vec()))
}

/// 解码文件并把最长边压到 `max_dim` 以内，直接返回 RGBA8 像素。
///
/// 与 [`decode_rgba8`] + [`crate::thumbnail::downscale_rgba8`] 的关键区别：**不会物化
/// 全尺寸 RGBA8**。解码器给出原色彩格式（`DynamicImage`，通常 RGB8 3 字节/像素）后立刻
/// `thumbnail` 缩小，只有缩小后的小图才转成 RGBA8。大图的解码峰值因此从
/// 「全尺寸 DynamicImage + 全尺寸 RGBA8 + 下采样输出」降为
/// 「全尺寸 DynamicImage + 小尺寸 RGBA8」，同时也省掉下采样时的整图临时拷贝。
///
/// 小图不会被放大（`thumbnail` 会把小图放大到边界框，纯粹浪费）。
pub fn decode_rgba8_scaled(
    path: &Path,
    max_dim: u32,
) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    let max_dim = max_dim.max(1);
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    let small = if img.width() <= max_dim && img.height() <= max_dim {
        img
    } else {
        img.thumbnail(max_dim, max_dim)
    };
    let rgba = small.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((w, h, rgba.into_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_decode_caps_the_longest_edge_and_never_upscales() {
        let mut img = image::RgbaImage::new(600, 400);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([(x % 251) as u8, (y % 251) as u8, 128, 255]);
        }
        let path = std::env::temp_dir().join("image_loader_core_scaled_decode.png");
        img.save(&path).expect("test png");

        let (w, h, rgba) = decode_rgba8_scaled(&path, 200).expect("scaled decode");
        assert_eq!((w, h), (200, 133));
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        let (w2, h2, _) = decode_rgba8_scaled(&path, 1000).expect("unscaled decode");
        assert_eq!((w2, h2), (600, 400), "small images must not be upscaled");

        std::fs::remove_file(&path).ok();
    }
}
