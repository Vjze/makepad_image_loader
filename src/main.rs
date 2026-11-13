// #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Linux/Wayland 下默认让合成器不要画系统标题栏（client-side decorations）。
    // 否则 KDE 画一条原生标题栏，Window 又因为 caption_bar 里有自定义按钮
    // （filetree_btn）再画一条，出现「标题栏下又多一行」。CSD 下 app 自绘的
    // caption_bar 就是唯一标题栏，和 Windows 上的行为一致。
    // 可用 MAKEPAD_WAYLAND_DECORATION=server 或 --wayland-decoration=server 覆盖。
    #[cfg(target_os = "linux")]
    {
        let overridden = std::env::var_os("MAKEPAD_WAYLAND_DECORATION").is_some()
            || std::env::args().any(|a| a.starts_with("--wayland-decoration="));
        if !overridden {
            // SAFETY: 进程刚进入 main，尚未创建任何线程；此时修改进程环境是安全的。
            unsafe { std::env::set_var("MAKEPAD_WAYLAND_DECORATION", "client") };
        }
    }
    // app_main! 在 app.rs 里生成的 fn main() 位于模块内部，并不是二进制入口；
    // 这里直接调用 app_main() 会跳过 run_app()，而新版 makepad 的 Linux HAL
    // 要求把进程主线程交给 HAL（expect_main_loop / run_main_loop），否则
    // hal_open 直接拒绝创建会话并报 "the HAL refused to open a session"。
    // run_app() 等价于宏生成的 main：主线程跑 HAL 事件循环，app 跑在独立线程。
    makepad_widgets::run_app(img_loader::app::app_main)
}
