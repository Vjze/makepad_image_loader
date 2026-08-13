use std::path::{Path, PathBuf};

pub(crate) fn is_image_file(f: String) -> bool {
    let images_exts: Vec<&str> = vec![
        ".png", ".jpeg", ".webp", ".pnm", ".ico", ".avif", ".jpg", ".gif", ".JPG", ".GIF", ".PNG",
        ".JPRG", ".WEBP", ".PNM", ".ICO", ".AVIF",
    ];
    for x in &images_exts {
        if f.ends_with(x) {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub path: String,
    pub name: String,
    pub image_idx: Option<usize>, // 在 image_paths 中的索引
    pub children: Vec<FileTreeNode>,
}

#[derive(Clone, Debug)]
pub struct FileTreeData {
    pub dir_path: PathBuf,
    pub root: FileTreeNode,
}

// 递归扫描目录，返回 (所有图片路径, 目录树)
pub(crate) fn build_image_tree(dir_path: &Path) -> (Vec<PathBuf>, FileTreeNode) {
    fn build_node(dir: &Path, image_paths: &mut Vec<PathBuf>) -> FileTreeNode {
        let mut children: Vec<FileTreeNode> = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(dir) {
            let mut entries: Vec<(String, std::fs::DirEntry)> = read_dir
                .filter_map(Result::ok)
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    (name, entry)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, entry) in entries {
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    children.push(build_node(&entry_path, image_paths));
                } else if entry_path.is_file() && is_image_file(name.clone()) {
                    let image_idx = image_paths.len();
                    image_paths.push(entry_path.clone());
                    children.push(FileTreeNode {
                        path: entry_path.to_string_lossy().to_string(),
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
    let mut image_paths: Vec<PathBuf> = Vec::new();
    let root = build_node(dir_path, &mut image_paths);
    (image_paths, root)
}
