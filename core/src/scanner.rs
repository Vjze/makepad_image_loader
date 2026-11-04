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

/// 流式扫描过程中产生的事件（框架无关，由 UI 层转成自己的消息）。
#[derive(Clone, Debug)]
pub enum ScanEvent {
    /// 已访问的目录项数量、当前正在处理的名字（进度条分子）。
    Progress { visited: usize, current: String },
    /// 发现一张图片：`index` 即原始图片索引，与文件树节点的 `image_idx` 一致。
    Found { index: usize, path: PathBuf },
}

/// 递归统计目录项总数，用作进度条分母。只列目录、不读文件内容，
/// 因此比解码/扫描本身便宜得多；统计口径与 `scan_directory_streaming`
/// 的 `visited` 完全一致（每个目录项计 1，子目录再递归累计）。
pub fn count_entries(dir_path: &Path) -> usize {
    fn count(dir: &Path) -> usize {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut n = 0;
        for entry in rd.filter_map(Result::ok) {
            n += 1;
            let p = entry.path();
            if p.is_dir() {
                n += count(&p);
            }
        }
        n
    }
    count(dir_path)
}

/// 流式递归扫描：每访问一个目录项回调一次 [`ScanEvent::Progress`]，每发现一张
/// 图片回调一次 [`ScanEvent::Found`]，最后返回构建好的目录树。
///
/// UI 层可以一边收 `Found` 一边往缩略图列表里追加（边读边写），同时用
/// `Progress` 驱动进度条，避免「一次性全量扫描完才刷新」造成的长时间无响应。
pub fn scan_directory_streaming(
    dir_path: &Path,
    emit: &mut dyn FnMut(ScanEvent),
) -> FileTreeNode {
    fn build(
        dir: &Path,
        next_idx: &mut usize,
        visited: &mut usize,
        emit: &mut dyn FnMut(ScanEvent),
    ) -> FileTreeNode {
        let mut children = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut entries: Vec<(String, std::fs::DirEntry)> = rd
                .filter_map(Result::ok)
                .map(|e| (e.file_name().to_string_lossy().to_string(), e))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, entry) in entries {
                *visited += 1;
                emit(ScanEvent::Progress {
                    visited: *visited,
                    current: name.clone(),
                });
                let p = entry.path();
                if p.is_dir() {
                    children.push(build(&p, next_idx, visited, emit));
                } else if p.is_file() && is_image_file(&name) {
                    let image_idx = *next_idx;
                    *next_idx += 1;
                    emit(ScanEvent::Found {
                        index: image_idx,
                        path: p.clone(),
                    });
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

    let mut next_idx = 0;
    let mut visited = 0;
    build(dir_path, &mut next_idx, &mut visited, emit)
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
