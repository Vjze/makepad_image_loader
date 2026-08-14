use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use makepad_widgets::*;
use crate::tree::FileTreeData;

/// 应用全局状态，挂在 Scope 上供各组件（widget）读取/修改。
/// 字段设为 `pub(crate)` 以便各 widget 模块在自身的 handle_event / WidgetMatchEvent 中访问。
#[derive(Debug)]
pub struct State {
    pub(crate) image_paths: Vec<PathBuf>,
    pub(crate) filtered_image_idxs: Vec<usize>,
    pub(crate) max_images_per_row: usize,
    pub(crate) current_image_idx: Option<usize>,
    pub(crate) in_slideshow: bool,
    pub(crate) file_tree_data: Option<FileTreeData>,
    pub(crate) file_tree_open: bool,
    pub(crate) loaded_image_idxs: HashSet<usize>,
    pub(crate) thumb_textures: HashMap<usize, Texture>,
    pub(crate) thumb_order: Vec<usize>,
    pub(crate) thumb_in_flight: HashSet<usize>,
    /// 文件夹加载代际：每次切换文件夹时递增，用于丢弃旧文件夹遗留的后台解码结果。
    pub(crate) generation: u64,
}

impl State {
    pub(crate) fn num_images(&self) -> usize {
        self.filtered_image_idxs.len()
    }

    pub(crate) fn num_rows(&self) -> usize {
        self.num_images().div_ceil(self.max_images_per_row)
    }

    pub(crate) fn first_image_idx_for_row(&self, row_idx: usize) -> usize {
        row_idx * self.max_images_per_row
    }

    pub(crate) fn num_images_for_row(&self, row_idx: usize) -> usize {
        let first_image_idx = self.first_image_idx_for_row(row_idx);
        let num_remaining_images = self.num_images() - first_image_idx;
        self.max_images_per_row.min(num_remaining_images)
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            image_paths: Vec::new(),
            filtered_image_idxs: Vec::new(),
            max_images_per_row: 4,
            current_image_idx: None,
            in_slideshow: false,
            file_tree_data: None,
            file_tree_open: false,
            loaded_image_idxs: HashSet::new(),
            thumb_textures: HashMap::new(),
            thumb_order: Vec::new(),
            thumb_in_flight: HashSet::new(),
            generation: 0,
        }
    }
}
