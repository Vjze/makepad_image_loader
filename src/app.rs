use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use makepad_widgets::*;

use crate::state::State;
use crate::actions::OpenImage;
use crate::tree::{build_image_tree, FileTreeData};

static DBG_DRAW_COUNT: AtomicUsize = AtomicUsize::new(0);
static DBG_TIMER_ID: AtomicU64 = AtomicU64::new(0);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let LEFT_ARROW = crate_resource("self://resources/left_arrow.svg");
    let RIGHT_ARROW = crate_resource("self://resources/right_arrow.svg");
    let LOOKING_GLASS = crate_resource("self://resources/looking_glass.svg");

    let scene_cyan = RadGradient {
            cx: 0.5 cy: 0.5 r: 0.5
            Stop { offset: 0 color: #x54d5ff opacity: 0.50 }
            Stop { offset: 0.5 color: #x2e7cff opacity: 0.15 }
            Stop { offset: 1 color: #x2e7cff opacity: 0.0 }
        }
        let scene_gold = RadGradient {
            cx: 0.5 cy: 0.5 r: 0.5
            Stop { offset: 0 color: #xffc86f opacity: 0.45 }
            Stop { offset: 0.55 color: #xff8f3a opacity: 0.12 }
            Stop { offset: 1 color: #xff8f3a opacity: 0.0 }
        }
        let scene_violet = RadGradient {
            cx: 0.5 cy: 0.5 r: 0.5
            Stop { offset: 0 color: #xd79cff opacity: 0.40 }
            Stop { offset: 0.55 color: #xf7f67ff opacity: 0.10 }
            Stop { offset: 1 color: #xf7f67ff opacity: 0.0 }
        }


        let CaptionChromeToggle = ButtonFlatterIcon {
                width: 36.0
                height: 28.0
                icon_walk: Walk {width: 16.0 height: 16.0}
                draw_bg +: {
                    color: #x474747
                    color_hover: #x525252
                    color_down: #x414141
                    border_radius: 4.0
                }
                draw_icon +: {
                    svg: crate_resource("self://src/resources/icon_sidebar_toggle.svg")
                    color: #xCBCBCB
                }
            }
    let State_bar = View {
        width: Fill,
        height: Fit,
        align: VCenter
        margin: Inset{ right: 5 }


        search_btn := Button {
            text: "打开文件夹",
        }
        all :=  Label {
            text: "共: 0张",
        }
        Filler {}

        slideshow_button := Button {
            text: "幻灯片模式"
        }
    }
    let MenuBar = View {
        width: Fill,
        height: Fit,
        align: VCenter
        margin: Inset{ left: 5 }
        Filler> {}
        State_bar {}
    }

    let ImageBrowser = View {
        width: Fill,
        height: Fill,
        flow: Down,
        MenuBar {}
        mod.widgets.ImageGrid {}
    }

    let SlideshowNavigateButton = Button {
        width: 50,
        height: Fill,
        draw_bg +: {
            color: #fff0,
            color_down: #fff2,
        }
        icon_walk +: { width: 9 },
        text: "",
        grab_key_focus: false,
    }

    let SlideshowOverlay = View {
        height: Fill,
        width: Fill,

        navigate_left := SlideshowNavigateButton {
            draw_icon +: { svg: LEFT_ARROW }
        }
        Filler {}
        navigate_right := SlideshowNavigateButton {
            draw_icon +: { svg: RIGHT_ARROW }
        }
    }

    let Slideshow = View {
        flow: Overlay,

        image_view := mod.widgets.ZoomableView {
            image := Image {
                width: Fill,
                height: Fill,
                fit: ImageFit.Smallest,
            }
        }

        overlay := SlideshowOverlay {}
    }

    // 侧边栏外壳：标题栏 + 文件树
    let FileTreeSideBar = RoundedView {
        width: Fill, height: Fill
        flow: Down
        header := View {
            width: Fill, height: Fit
            padding: Inset{ left: 10.0, top: 8.0, right: 10.0, bottom: 6.0 }
            flow: Right
            align: VCenter
            title_label := Label {
                text: "文件列表"
                draw_text +: {
                    text_style +: { font_size: 11.0 }
                    color: #c
                }
            }
        }
        mod.widgets.FileTreePanel {}
    }

    // ContentView：主内容区，包含 PageFlip
    let ContentView = View {
        width: Fill, height: Fill
        page_flip := PageFlip {
            width: Fill, height: Fill
            active_page: @image_browser
            image_browser := ImageBrowser {}
            slideshow := Slideshow {}
        }
    }

    // 整个应用背景
    let AppBody = RoundedView {
        width: Fill, height: Fill
        flow: Overlay
        bg_scene := Vector {
                        width: Fill, height: Fill
                        viewbox: vec4(0 0 1400 900)
                        Circle { cx: 200 cy: 180 r: 380 fill: scene_cyan }
                        Circle { cx: 1180 cy: 200 r: 320 fill: scene_gold }
                        Circle { cx: 700 cy: 680 r: 420 fill: scene_violet }
                    }
        split_view := Splitter {
            width: Fill, height: Fill
            axis: SplitterAxis.Horizontal
            align: SplitterAlign.FromA(0.0)
            size: 3.0
            min_vertical: 0.0
            min_horizontal: 0.0
            draw_bg +: {
                color: #xff0000
                color_hover: #xff0000
                color_drag: #xff0000
                border_radius: 1.5
                splitter_pad: 1.5
            }
            a: FileTreeSideBar {}
            b: ContentView {}
        }
    }
    let MolyDesktopButton = DesktopButton {
        draw_bg +: {
            color: uniform(#000)
            color_hover: uniform(#0004)
            color_down: uniform(#0006)
        }
    }
    startup() do #(App::script_component(vm)){
        ui: Root {
            main_window := Window {
                caption_bar := SolidView {
                            width: Fill
                            height: Fit
                            flow: Right
                            left_controls := View {
                                width: Fit
                                height: Fit
                                flow: Right
                                align: Align {x: 0.0 y: 0.5}
                                margin: Inset {left: 8.0 top: 0.0}
                                filetree_btn := CaptionChromeToggle {}
                            }
                            caption_label := View {
                                            width: Fill
                                            height: Fill
                                            align: VCenter
                                        }
                                        windows_buttons := View {
                                            width: Fit height: Fit
                                            min := MolyDesktopButton {
                                                draw_bg +: {button_type: DesktopButtonType.WindowsMin}
                                            }
                                            max := MolyDesktopButton {
                                                draw_bg +: {button_type: DesktopButtonType.WindowsMax}
                                            }
                                            close := MolyDesktopButton {
                                                draw_bg +: {button_type: DesktopButtonType.WindowsClose}
                                            }
                                        }
                }
                window.inner_size: vec2(1280, 768)
                window.title: "Image_Loader"
                body: AppBody {}
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    state: State,
}

impl App {
    fn load_image_paths(&mut self, cx: &mut Cx) {
        self.state.image_paths.clear();
        self.state.loaded_image_idxs.clear();
        self.state.thumb_textures.clear();
        self.state.thumb_order.clear();
        self.state.thumb_in_flight.clear();
        // 递增代际，使旧文件夹遗留的后台解码结果在回传时被丢弃。
        self.state.generation = self.state.generation.wrapping_add(1);
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            let path = p.to_path_buf();
            let dir_path = path.clone();
            let (image_paths, root) = build_image_tree(&path);
            self.state.image_paths = image_paths;
            self.state.file_tree_data = Some(FileTreeData {
                dir_path: dir_path.clone(),
                root,
            });
            self.state.file_tree_open = true;
            let splitter = self.ui.splitter(cx, ids!(split_view));
            splitter.set_align(cx, SplitterAlign::FromA(200.0));
            self.ui.redraw(cx);
            self.ui
                .label(cx, ids!(all))
                .set_text(cx, &format!("共: {}张", self.state.image_paths.len()));
            // 当前无搜索输入框，直接显示全部图片
            self.filter_image_paths(cx, "");
        }
    }
    pub fn filter_image_paths(&mut self, cx: &mut Cx, query: &str) {
        self.state.filtered_image_idxs.clear();
        for (image_idx, image_path) in self.state.image_paths.iter().enumerate() {
            if image_path.to_str().unwrap().contains(query) {
                self.state.filtered_image_idxs.push(image_idx);
            }
        }
        if self.state.filtered_image_idxs.is_empty() {
            self.set_current_image(cx, None);
        } else {
            self.set_current_image(cx, Some(0));
        }
    }
    fn open_slideshow(&mut self, cx: &mut Cx) {
        self.state.in_slideshow = true;
        self.ui
            .page_flip(cx, ids!(page_flip))
            .set_active_page(cx, live_id!(slideshow));
    }

    fn navigate_left(&mut self, cx: &mut Cx) {
        if let Some(image_idx) = self.state.current_image_idx.filter(|&idx| idx > 0) {
            self.set_current_image(cx, Some(image_idx - 1));
        }
    }

    fn navigate_right(&mut self, cx: &mut Cx) {
        if let Some(image_idx) = self
            .state
            .current_image_idx
            .filter(|image_idx| image_idx + 1 < self.state.num_images())
        {
            self.set_current_image(cx, Some(image_idx + 1));
        }
    }

    fn set_current_image(&mut self, cx: &mut Cx, image_idx: Option<usize>) {
        self.state.current_image_idx = image_idx;
        // 重置缩放/平移（缩放状态由 ZoomableView 内部管理）。
        if let Some(mut zv) = self
            .ui
            .widget(cx, ids!(slideshow.image_view))
            .borrow_mut::<crate::widgets::zoomable_view::ZoomableView>()
        {
            zv.reset_zoom(cx);
        }
        let image = self.ui.image(cx, ids!(slideshow.image_view.image));
        if let Some(image_idx) = self.state.current_image_idx {
            let filtered_image_idx = self.state.filtered_image_idxs[image_idx];
            let image_path = &self.state.image_paths[filtered_image_idx];
            let _ = image.load_image_file_by_path_async(cx, image_path);
        } else {
            let _ = image.load_image_dep_by_path(cx, "");
        }
        self.ui.redraw(cx);
    }

    // 仅负责协调组件发来的 OpenImage 命令。
    // target_idx 是“原始图片索引”（image_paths 下标），在此转成过滤后的下标。
    fn handle_open_image(&mut self, cx: &mut Cx, target_idx: usize) {
        if target_idx >= self.state.image_paths.len() {
            return;
        }
        if let Some(pos) = self
            .state
            .filtered_image_idxs
            .iter()
            .position(|&i| i == target_idx)
        {
            self.set_current_image(cx, Some(pos));
        } else {
            // 目标图片不在当前过滤结果中：直接加载该图（current_image_idx 置空表示非过滤定位）
            self.state.current_image_idx = None;
            let image = self.ui.image(cx, ids!(slideshow.image_view.image));
            let _ = image.load_image_file_by_path_async(cx, &self.state.image_paths[target_idx]);
        }
        self.open_slideshow(cx);
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        script_eval!(vm, {
            mod.theme = mod.themes.dark
        });
        makepad_widgets::widgets_mod(vm);
        crate::script_mod(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        if let Event::KeyDown(ke) = event {
            match ke.key_code {
                KeyCode::Escape => {
                    self.state.in_slideshow = false;
                    self.ui
                        .page_flip(cx, ids!(page_flip))
                        .set_active_page(cx, live_id!(image_browser));
                }
                KeyCode::ArrowLeft if self.state.in_slideshow => self.navigate_left(cx),
                KeyCode::ArrowRight if self.state.in_slideshow => self.navigate_right(cx),
                _ => {}
            }
        }
        if matches!(event, Event::Draw(_))
            && DBG_DRAW_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                DBG_TIMER_ID.store(cx.start_timeout(3.0).0, Ordering::Relaxed);
            }
        let mut scope = Scope::with_data(&mut self.state);
        self.ui.handle_event(cx, event, &mut scope);
        if let Event::WindowDragQuery(dq) = event {
            let main_window = self.ui.window(cx, ids!(main_window));
            if Some(dq.window_id) == main_window.window_id() {
                let sidebar_rect = self.ui.button(cx, ids!(filetree_btn)).area().rect(cx);

                if sidebar_rect.contains(dq.abs) {
                    dq.response.set(WindowDragQueryResponse::Client);
                    cx.set_cursor(MouseCursor::Default);
                }
            }
        }
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // 组件各自处理完自身 action 后，通过 OpenImage 命令请求 App 协调。
        // 注意：cx.widget_action 发出的顶层 action 是 WidgetAction，
        // 真正的 OpenImage 包在 WidgetAction.action 里，需要两层 downcast。
        for action in actions.iter() {
            if let Some(wa) = action.downcast_ref::<WidgetAction>()
                && let Some(open) = wa.action.downcast_ref::<OpenImage>() {
                    self.handle_open_image(cx, open.image_index);
                }
        }
        // App 壳控件（无独立组件）直接响应
        if self.ui.button(cx, ids!(search_btn)).clicked(actions) {
            self.load_image_paths(cx);
        }
        if self.ui.button(cx, ids!(slideshow_button)).clicked(actions) {
            self.open_slideshow(cx);
        }
        if self.ui.button(cx, ids!(filetree_btn)).clicked(actions)
            && self.state.file_tree_data.is_some() {
                self.state.file_tree_open = !self.state.file_tree_open;
                let align = if self.state.file_tree_open {
                    SplitterAlign::FromA(200.0)
                } else {
                    SplitterAlign::FromA(0.0)
                };
                let splitter = self.ui.splitter(cx, ids!(split_view));
                splitter.set_align(cx, align);
                self.ui.redraw(cx);
            }
        if self.ui.button(cx, ids!(navigate_left)).clicked(actions) {
            self.navigate_left(cx);
        }
        if self.ui.button(cx, ids!(navigate_right)).clicked(actions) {
            self.navigate_right(cx);
        }
    }
}

app_main!(App);
