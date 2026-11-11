//! 把 `image_loader_core` 的目录树摊平成「可显示的行」，供虚拟列表消费。
//! 对应 Makepad 版 `FileTreePanel::draw_tree_node` 的展开/折叠行为。

use std::collections::HashSet;

use image_loader_core::scanner::FileTreeNode;

use crate::State;

#[derive(Clone, Debug)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub image_idx: Option<usize>,
    pub is_dir: bool,
    pub open: bool,
}

pub fn rebuild(s: &mut State) {
    s.tree_rows.clear();
    let Some(root) = s.tree.as_ref().map(|t| t.root.clone()) else {
        return;
    };
    // 根目录默认展开，与 Makepad 版 set_folder_is_open(root, true) 一致。
    s.expanded.insert(root.path.clone());
    let expanded = s.expanded.clone();
    walk(&root, 0, &expanded, &mut s.tree_rows);
}

fn walk(
    node: &FileTreeNode,
    depth: usize,
    expanded: &HashSet<String>,
    out: &mut Vec<TreeRow>,
) {
    let is_dir = node.image_idx.is_none();
    let open = is_dir && expanded.contains(&node.path);
    out.push(TreeRow {
        depth,
        name: node.name.clone(),
        path: node.path.clone(),
        image_idx: node.image_idx,
        is_dir,
        open,
    });
    if open {
        for child in &node.children {
            walk(child, depth + 1, expanded, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image_loader_core::scanner::FileTreeData;
    use std::path::PathBuf;

    fn file(path: &str, name: &str, idx: usize) -> FileTreeNode {
        FileTreeNode {
            path: path.into(),
            name: name.into(),
            image_idx: Some(idx),
            children: Vec::new(),
        }
    }

    #[test]
    fn expanding_a_folder_reveals_its_children() {
        let mut s = crate::init();
        let sub = FileTreeNode {
            path: "root/sub".into(),
            name: "sub".into(),
            image_idx: None,
            children: vec![file("root/sub/a.png", "a.png", 0)],
        };
        s.tree = Some(FileTreeData {
            dir_path: PathBuf::from("root"),
            root: FileTreeNode {
                path: "root".into(),
                name: "root".into(),
                image_idx: None,
                children: vec![sub, file("root/b.png", "b.png", 1)],
            },
        });

        rebuild(&mut s);
        // 根目录默认展开，子目录默认折叠：root / sub / b.png
        assert_eq!(s.tree_rows.len(), 3);
        assert!(s.tree_rows[0].open);

        s.expanded.insert("root/sub".into());
        rebuild(&mut s);
        assert_eq!(s.tree_rows.len(), 4);
        assert_eq!(s.tree_rows[2].name, "a.png");
    }
}
