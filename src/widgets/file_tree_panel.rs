use makepad_widgets::*;
use crate::actions::OpenImage;
use crate::state::State;
use crate::tree::FileTreeNode;

// ---------------------------------------------------------------------------
// FileTreePanel：文件树组件，点击文件节点 → 发出 OpenImage 命令
//   参照官方 studio DesktopFileTree：#[deref] view: View 包裹 FileTree
// ---------------------------------------------------------------------------
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.FileTreePanelBase = #(FileTreePanel::register_widget(vm))
    mod.widgets.FileTreePanel = set_type_default() do mod.widgets.FileTreePanelBase{
        width: Fill,
        height: Fill,
        flow: Down,
        file_tree := FileTree {
            width: Fill,
            height: Fill,
            node_height: 24.0,
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FileTreePanel {
    #[deref]
    view: View,
}

impl Widget for FileTreePanel {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        crate::dbg("FILETREE draw_walk called");
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut file_tree) = step.as_file_tree().borrow_mut() {
                file_tree.set_folder_is_open(cx, live_id!(root), true, Animate::No);
                if let Some(state) = scope.data.get::<State>()
                    && let Some(ref ft_data) = state.file_tree_data {
                        Self::draw_tree_node(cx, &mut file_tree, &ft_data.root, live_id!(root));
                    }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        let file_tree = self.view.file_tree(cx, ids!(file_tree));

        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event
            && let Some(item) = actions.find_widget_action(file_tree.widget_uid())
                && let FileTreeAction::FileClicked(file_id) = item.cast() {
                    let idx = file_id.0 as usize - 1;
                    let state = scope.data.get::<State>().unwrap();
                    if idx < state.image_paths.len() {
                        cx.widget_action(uid, OpenImage { image_index: idx });
                    }
                }
    }
}

impl FileTreePanel {
    fn draw_tree_node(
        cx: &mut Cx2d,
        file_tree: &mut FileTree,
        node: &FileTreeNode,
        node_id: LiveId,
    ) {
        if let Some(image_idx) = node.image_idx {
            file_tree.file(cx, LiveId(image_idx as u64 + 1), &node.name);
        } else {
            if !file_tree.is_folder(node_id) {
                file_tree.set_folder_is_open(cx, node_id, true, Animate::No);
            }
            if file_tree.begin_folder(cx, node_id, &node.name).is_ok() {
                for child in &node.children {
                    let child_id = match child.image_idx {
                        Some(image_idx) => LiveId(image_idx as u64 + 1),
                        None => LiveId::from_str(&child.path),
                    };
                    Self::draw_tree_node(cx, file_tree, child, child_id);
                }
                file_tree.end_folder();
            }
        }
    }
}
