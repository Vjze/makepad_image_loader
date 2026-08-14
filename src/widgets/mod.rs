use makepad_widgets::*;
pub mod image_card;
pub mod image_grid;
pub mod zoomable_view;
pub mod file_tree_panel;
pub mod local_file_tree;
pub fn script_mod(vm: &mut ScriptVm) {
    local_file_tree::script_mod(vm);
    file_tree_panel::script_mod(vm);
    image_card::script_mod(vm);
    image_grid::script_mod(vm);
    zoomable_view::script_mod(vm);
}