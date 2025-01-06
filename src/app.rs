use std::sync::{Arc, Mutex};

use makepad_widgets::*;
use tokio::runtime::Runtime;
live_design!(
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    App = {{App}} {
            ui: <Root>{
                main_window = <Window>{
                    caption_bar = {
                        visible: true,
                        margin: {left: -100},
                        caption_label = { label = {text: "Image_Loader"} }
                    },
                    body = <View>{
                        flow: Down,
                        spacing: 5.0,
                        show_bg: true,
                        margin: 5.0,
                        // draw_bg: {
                        //     fn pixel(self) -> vec4 {
                        //         let color1 = #98FB98;  // 浅绿色
                        //         let color2 = #87CEEB;  // 天蓝色
                        //         return mix(color1, color2, self.pos.y);
                        //     }
                        // }
                        <View>{
                            width: Fill,
                            height: Fill,
                            // margin: 10,
                            image = <Image> {
                                width: Fill,
                                height: Fill,
                                fit: Contain
                            }
                        }
                        <View>{
                            spacing: 0,
                            show_bg: false,
                            width: Fill,
                            height: Fit,
                            flow: Right,
                            align: { x: 0.0, y: 1.0 },
                            padding: { bottom: 2 },
                            <View> {
                                width: 120,
                                height: Fit,
                                button = <Button> {
                                    width: Fill,
                                    height: 40,
                                    text: "选择文件夹"
                                    draw_text:{
                                        color:#fff,
                                        text_style: {
                                            font_size: 14
                                        }
                                    }
                                    draw_bg: {
                                        uniform border_radius: 5.0
                                    }
                                }
                            }
                            <View> {
                                width: 100,
                                height: Fit,
                                margin: { left: 10, right: 10 },
                                pre_button = <Button> {
                                    width: Fill,
                                    height: 40,
                                    text: "上一张"
                                    draw_text:{
                                        color:#fff,
                                        text_style: {
                                            font_size: 14
                                        }
                                    }
                                }
                            }
                            <View> {
                                width: 100,
                                height: Fit,
                                next_button = <Button> {
                                    width: Fill,
                                    height: 40,
                                    text: "下一张"
                                    draw_text:{
                                        color:#fff,
                                        text_style: {
                                            font_size: 14
                                        }
                                    }
                                }
                            }
                            <View> {
                                width: Fill,
                                height: Fit,
                                flow: Right,
                                align: { x: 1.0, y: 1.0 },
                                margin: { left: 20 },
                                all = <Label> {
                                    margin: { right: 10 },
                                    draw_text: {
                                        text_style: {
                                            font_size: 16,
                                        }
                                    }
                                    text: ""
                                }
                                now = <Label> {
                                    margin: { right: 10 },
                                    draw_text: {
                                        text_style: {
                                            font_size: 16,
                                        }
                                    }
                                    text: ""
                                }
                                pixel = <Label> {
                                    draw_text: {
                                        text_style: {
                                            font_size: 16,
                                        }
                                    }
                                    text: ""
                                }
                            }
                        }
                    }

                }
            }
        }
);
app_main!(App);

#[derive(Live, LiveHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    now: usize,
    #[rust]
    list: Arc<Mutex<Vec<String>>>,
    #[rust(Runtime::new().unwrap())]
    rt: Runtime,
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::KeyDown(key_event) => {
                if key_event.key_code == KeyCode::ArrowUp {
                    self.pre(cx);
                } else if key_event.key_code == KeyCode::ArrowDown {
                    self.next(cx);
                } else if key_event.key_code == KeyCode::ArrowLeft {
                    self.pre(cx);
                } else if key_event.key_code == KeyCode::ArrowRight {
                    self.next(cx);
                }
            }
            _ => (),
        };
        self.match_event(cx, event);
        self.ui_runner()
            .handle(cx, event, &mut Scope::empty(), self);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        crate::makepad_widgets::live_design(cx);
    }
}
impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(id!(button)).clicked(&actions) {
            self.serch(cx);
        }
        if self.ui.button(id!(next_button)).clicked(&actions) {
            self.next(cx);
        }
        if self.ui.button(id!(pre_button)).clicked(&actions) {
            self.pre(cx);
        }
        self.ui.redraw(cx);
    }
}
impl App {
    fn serch(&mut self, _cx: &mut Cx) {
        // 在主线程打开文件对话框
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            let path = p.to_path_buf();
            let list = Arc::clone(&self.list);
            let ui = self.ui_runner();

            // 在后台线程处理文件搜索
            let task = async move {
                let mut list_vec = vec![];
                for entry in walkdir::WalkDir::new(path)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| is_image_file(e.file_name().to_str().unwrap().to_string()))
                {
                    let file_path = entry.path().display().to_string();
                    list_vec.push(file_path);
                }
                list_vec
            };

            self.rt.spawn(async move {
                let lists = task.await;
                let len = lists.len();
                *list.lock().unwrap() = lists;

                ui.defer(move |app, cx, _| {
                    app.ui.label(id!(all)).set_text(&format!("共: {}张", len));
                    if len > 0 {
                        // 更新当前索引
                        app.now = 1;
                        // 获取第一张图片
                        if let Ok(list) = app.list.try_lock() {
                            if let Some(path) = list.get(0) {
                                // 更新当前图片索引文本
                                app.ui
                                    .label(id!(now))
                                    .set_text(&format!("当前第: {}张", app.now));
                                // 加载图片
                                let i = app.ui.image(id!(image));
                                let _ = i.load_image_file_by_path(cx, path);
                                // 更新分辨率信息
                                if let Some(size) = i.size_in_pixels(cx) {
                                    let pixel = format!("分辨率:{}*{}", size.0, size.1);
                                    app.ui.label(id!(pixel)).set_text(&pixel);
                                }
                            }
                        }
                    }
                });
            });
        
        }
    }
    fn pre(&mut self, cx: &mut Cx) {
        let l = self.list.clone();
        let list = l.try_lock().unwrap();
        let mut now = 0;
        self.now -= 1;
        if self.now <= 0 {
            now = list.len() - 1;
            self.now = list.len();
        } else {
            let x = self.now.clone() - 1;
            now = x as usize;
        }
        let now_text = "当前第: ".to_string() + self.now.to_string().as_str() + "张";
        self.ui.label(id!(now)).set_text_and_redraw(cx, &now_text);
        let i = self.ui.image(id!(image));
        let path = list.get(now as usize).unwrap();
        let _ = i.load_image_file_by_path(cx, &path);
        let size = self.ui.image(id!(image)).size_in_pixels(cx).unwrap();
        let width = size.0.to_string();
        let height = size.1.to_string();
        let pixel = "分辨率:".to_string() + width.as_str() + "*" + height.as_str();
        self.ui.label(id!(pixel)).set_text_and_redraw(cx, &pixel);
    }
    fn next(&mut self, cx: &mut Cx) {
        let l = self.list.clone();
        let list = l.try_lock().unwrap();
        let mut now = 0;
        self.now += 1;
        if self.now - 1 >= list.len() {
            now = 0;
            self.now = 1;
        } else {
            let x = self.now.clone() - 1;
            now = x as usize;
        }

        let now_text = "当前第: ".to_string() + self.now.to_string().as_str() + "张";
        self.ui.label(id!(now)).set_text_and_redraw(cx, &now_text);
        let i = self.ui.image(id!(image));
        let path = list.get(now).unwrap();
        let _ = i.load_image_file_by_path(cx, &path);
        let size = self.ui.image(id!(image)).size_in_pixels(cx).unwrap();
        let width = size.0.to_string();
        let height = size.1.to_string();
        let pixel = "分辨率:".to_string() + width.as_str() + "*" + height.as_str();
        self.ui.label(id!(pixel)).set_text_and_redraw(cx, &pixel);
    }
}

fn is_image_file(f: String) -> bool {
    let images_exts: Vec<&str> = vec![
        ".png", ".jpeg", ".webp", ".pnm", ".ico", ".avif", ".jpg", ".gif", ".JPG", ".GIF", ".PNG",
        ".JPRG", ".WEBP", ".PNM", ".ICO", ".AVIF",
    ];
    for x in &images_exts {
        if f.ends_with(x) {
            return true;
        }
    }
    return false;
}
