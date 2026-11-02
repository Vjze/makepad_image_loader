//! 共享逻辑 crate：文件夹扫描、解码、缩略图下采样与 LRU 缩略图缓存。
//!
//! 这套逻辑与具体 UI 框架无关，Makepad 版与 Xilem 版 UI 都复用这里：
//! - [`scanner`]：递归扫描目录、收集图片路径、构建文件树。
//! - [`decode`]：把图片文件解码为 RGBA8 像素。
//! - [`thumbnail`]：缩略图最长边下采样（盒式平均）与像素格式转换。
//! - [`cache`]：后台解码 + LRU 淘汰 + 文件夹代际（generation）丢弃的缩略图缓存。

pub mod scanner;
pub mod decode;
pub mod thumbnail;
pub mod cache;
