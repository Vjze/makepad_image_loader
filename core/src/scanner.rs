//! 文件夹扫描与文件树构建（框架无关）。
//!
//! 这是原来 Makepad 版 `src/tree.rs` 抽出来的公共逻辑：递归扫描目录，收集所有图片路径，
//! 并构建一棵可直接被 UI 渲染的文件树。两个 UI（Makepad / Xilem）都复用这里。

use std::path::{Path, PathBuf};

/// 支持的图片扩展名（小写比较）。
pub const IMAGE_EXTS: &[&str] = &[
    ".png", ".jpeg", ".jpg", ".webp", ".pnm", ".ico", ".avif", ".gif", ".bmp", ".tiff", ".tif",
];

/// 判断文件名是否为图片（大小写不敏感）。
pub fn is_image_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS.iter().any(|e| lower.ends_with(e))
}

/// 文件树节点。文件夹节点的 `image_idx` 为 `None`，文件节点为 `Some(图片索引)`。
#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub path: String,
    pub name: String,
    pub image_idx: Option<usize>,
    pub children: Vec<FileTreeNode>,
}

/// 一次扫描的结果：全部图片路径（按发现顺序）+ 目录树。
#[derive(Clone, Debug)]
pub struct FileTreeData {
    pub dir_path: PathBuf,
    pub root: FileTreeNode,
}

/// `scan_directory` 的返回：图片路径列表 + 文件树。
#[derive(Clone, Debug)]
pub struct ScanResult {
    pub image_paths: Vec<PathBuf>,
    pub tree: FileTreeData,
}

/// 递归扫描目录，返回所有图片路径与文件树。
///
/// `image_paths` 的顺序即为「原始图片索引」(`image_idx`) 的语义，UI 层过滤/翻页都基于它。
pub fn scan_directory(dir_path: &Path) -> ScanResult {
    fn build(dir: &Path, paths: &mut Vec<PathBuf>) -> FileTreeNode {
        let mut children = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut entries: Vec<(String, std::fs::DirEntry)> = rd
                .filter_map(Result::ok)
                .map(|e| (e.file_name().to_string_lossy().to_string(), e))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, entry) in entries {
                let p = entry.path();
                if p.is_dir() {
                    children.push(build(&p, paths));
                } else if p.is_file() && is_image_file(&name) {
                    let image_idx = paths.len();
                    paths.push(p.clone());
                    children.push(FileTreeNode {
                        path: p.to_string_lossy().to_string(),
                        name,
                        image_idx: Some(image_idx),
                        children: Vec::new(),
                    });
                }
            }
        }
        FileTreeNode {
            path: dir.to_string_lossy().to_string(),
            name: dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string()),
            image_idx: None,
            children,
        }
    }

    let mut image_paths = Vec::new();
    let root = build(dir_path, &mut image_paths);
    ScanResult {
        image_paths,
        tree: FileTreeData {
            dir_path: dir_path.to_path_buf(),
            root,
        },
    }
}
