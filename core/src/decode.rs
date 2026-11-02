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
pub fn decode_rgba8(path: &Path) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    let img = decode_image(path)?;
    // 先转成 RGBA8 再取数据，统一像素布局，避免各后端格式差异。
    let rgba = img.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((w, h, rgba.into_vec()))
}
