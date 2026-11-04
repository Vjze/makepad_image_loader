use makepad_widgets::*;

// 调试日志：写入固定路径的 dbg.log（带 .gitkeep 占位）
pub(crate) fn dbg(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open("C:/Users/Administrator/AppData/Local/Temp/opencode/dbg.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}

pub mod state;
pub mod actions;
pub mod tree;
pub mod thumb;
pub mod scan_worker;
pub mod widgets;
pub mod app;
pub fn script_mod(vm: &mut ScriptVm) {
    widgets::script_mod(vm);
}