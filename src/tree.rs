// 目录扫描与文件树逻辑已抽到公共 crate `image_loader_core`（Makepad 版与 Xilem 版共用）。
// 这里仅做再导出，保持旧的引用路径（crate::tree::*）不变。
pub use image_loader_core::scanner::{is_image_file, scan_directory, FileTreeData, FileTreeNode};
