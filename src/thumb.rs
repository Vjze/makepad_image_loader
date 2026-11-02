// 缩略图解码 / 下采样逻辑已抽到公共 crate `image_loader_core`（Makepad 版与 Xilem 版共用）。
// 这里仅做再导出与别名，保持旧的引用路径（crate::thumb::*）不变。
pub use image_loader_core::thumbnail::{downscale_rgba8, rgba8_to_argb_u32, THUMB_MAX_DIM};

// 与 core 的缓存参数保持一致（仅别名，避免调用方到处写完整路径）。
pub(crate) const THUMB_MAX_IN_FLIGHT: usize = image_loader_core::thumbnail::THUMB_MAX_IN_FLIGHT;
pub(crate) const THUMB_MAX_TEXTURES: usize = image_loader_core::thumbnail::THUMB_MAX_BITMAPS;
