use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
static DBG_DRAW_COUNT: AtomicUsize = AtomicUsize::new(0);
static DBG_TIMER_ID: AtomicU64 = AtomicU64::new(0);
static DBG_CAP1: AtomicBool = AtomicBool::new(false);
static ZOOM_FACTOR: AtomicU64 = AtomicU64::new(f64::to_bits(1.0));
static ZOOM_PAN_X: AtomicU64 = AtomicU64::new(f64::to_bits(0.0));
static ZOOM_PAN_Y: AtomicU64 = AtomicU64::new(f64::to_bits(0.0));
#[allow(dead_code)]
fn dbg(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open("C:/Users/Administrator/AppData/Local/Temp/opencode/dbg.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}
use makepad_widgets::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

    let SearchBox = View {
        width: Fill,
        height: Fit,
        align: VCenter
        margin: Inset{ left: 5 }

        Icon {
            icon_walk: Walk{ width: 12.0 }
            draw_icon +: {
                color: #8,
                svg: LOOKING_GLASS
            }
        }

        query := TextInput {
            empty_text: "Search",
            draw_text +: {
                text_style +: { font_size: 10 },
                color: #8
            }
        }
        Filler {}
        all :=  Label {
            text: "共: 0张",
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
        Filler {}
        filetree_btn := Button {
            text: "文件树"
        }
        slideshow_button := Button {
            text: "幻灯片模式"
        }
    }
    let MenuBar = View {
        width: Fill,
        height: Fit,
        align: VCenter
        margin: Inset{ left: 5 }
        SearchBox {}
        Filler> {}
        State_bar {}
    }

    // ------------------------------------------------------------------
    // ImageCardWidget：自定义 Widget，包装固定尺寸的卡片 View
    // - hover 动画由 View 的 Animator + handle_event 原生处理
    // - 点击由 Widget::handle_event 检测 FingerUp 并发射 ImageClicked
    // - 卡片固定 250×266，图片 240×240 → 间距统一
    // ------------------------------------------------------------------
    mod.widgets.ImageCardWidgetBase = #(ImageCardWidget::register_widget(vm))
    mod.widgets.ImageCardWidget = set_type_default() do mod.widgets.ImageCardWidgetBase{
        ..mod.widgets. RoundedView {
        width: 250,
        height: 250,
        flow: Down,
        align: Center,
        // animator: Animator {
        //     hover: {
        //         default: @off
        //         off: AnimatorState {
        //             cursor: MouseCursor.Default
        //         }
        //         on: AnimatorState {
        //             cursor: MouseCursor.Hand
        //         }
        //     }
        // }
       
            // align: Center,
            animator: Animator {
                    ignore_missing: true
                    hover: {
                        default: @off
                        off: AnimatorState {
                            from: {all: Forward {duration: 0.08}}
                            apply: {
                                width: 250,
                                height: 250,
                                draw_bg: {hover: 0.0}
                            }
                        }
                        on: AnimatorState {
                            cursor: MouseCursor.Hand
                            from: {all: Snap}
                            apply: {
                                width: 265,
                                height: 265,
                                draw_bg: {hover: 1.0}
                            }
                        }
                    }
                }
            
            draw_bg +: {
                border_size: uniform(1.0)
                border_radius: uniform(12.5)
            }

            thumb := Image {
                width: Fill,
                height: Fill,
                fit: ImageFit.Smallest,
                margin: Inset{ left: 8.0, top: 8.0, right: 8.0, bottom: 8.0 }
            }
        }
    }

    mod.widgets.ImageRowBase = #(ImageRow::register_widget(vm))
    mod.widgets.ImageRow = set_type_default() do mod.widgets.ImageRowBase{
        width: Fill,
        height: 250,
        PortalList {
            height: 250,
            flow: Right,
            spacing: 12.0,
            ImageCard := mod.widgets.ImageCardWidget {}
        }
    }

    mod.widgets.ImageGridBase = #(ImageGrid::register_widget(vm))
    mod.widgets.ImageGrid = set_type_default() do mod.widgets.ImageGridBase{
        width: Fill,
        height: Fill,
        PortalList {
            flow: Down,
            spacing: 0.0,
            ImageRow := mod.widgets.ImageRow {}
        }
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

    mod.widgets.ZoomableViewBase = #(ZoomableView::register_widget(vm))
    mod.widgets.ZoomableView = set_type_default() do mod.widgets.ZoomableViewBase{
        width: Fill,
        height: Fill,
        image := Image {
            width: Fill,
            height: Fill,
            fit: ImageFit.Smallest,
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

    // FileTreePanel：参照 shell_demo RemoteFileTreeView 模式
    //   #[live] pub file_tree: FileTree 直接绑定
    //   draw_walk 用 while .is_step() 两步法在树之间填充节点（DemoFileTree 模式）
    mod.widgets.FileTreePanelBase = #(FileTreePanel::register_widget(vm))
    mod.widgets.FileTreePanel = set_type_default() do mod.widgets.FileTreePanelBase{
        file_tree: FileTree {
            node_height: 24.0
        }
    }

    // 侧边栏外壳：标题栏 + 文件树（标题栏放 View flow 里，树由 FileTreePanel 自行填充）
    // RoundedView 自带 draw_bg shader + show_bg，普通 View 的 draw_bg 无 shader 画不出背景
    let FileTreeSideBar = RoundedView {
        width: 200, height: Fill
        draw_bg +: {
            color: #x16181c
            border_radius: 0.0
        }
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
            Filler {}
            collapse_btn := Button {
                text: "✕"
                width: Fit, height: Fit
                draw_text +: { color: #8 }
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

    // SplitView：使用 Splitter 的 a/b 命名子槽做左右分栏
    //   a: file_tree_panel (侧边栏), b: content_view (主内容)
    //   按钮通过 set_align 控制展开/收起
    mod.widgets.SplitView = Splitter {
        width: Fill, height: Fill
        axis: SplitterAxis.Horizontal
        align: SplitterAlign.FromA(200.0)
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

    // 整个应用背景：RoundedView 才有带 shader 的 draw_bg（普通 View/KeyboardView 的 draw_bg 无 shader）
    let AppBody = RoundedView {
        width: Fill, height: Fill
        flow: Down
        draw_bg +: {
            color: #x0b0d14
            border_radius: 0.0
        }
        split_view := Splitter {
            width: Fill, height: Fill
            axis: SplitterAxis.Horizontal
            align: SplitterAlign.FromA(200.0)
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

    startup() do #(App::script_component(vm)){
        ui: Root {
            main_window := Window {
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
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            let path = p.to_path_buf();
            let dir_path = path.clone();
            // 递归构建图片路径列表 + 文件树（子文件夹/子文件全部展开显示）
            let (image_paths, root) = build_image_tree(&path);
            self.state.image_paths = image_paths;
            self.state.file_tree_data = Some(FileTreeData {
                dir_path: dir_path.clone(),
                root,
            });
            self.state.file_tree_open = true; // 打开文件夹后自动展开
            let mut splitter = self.ui.splitter(cx, ids!(split_view));
            splitter.set_align(cx, SplitterAlign::FromA(200.0));
            self.ui.redraw(cx);
            self.ui
                .label(cx, ids!(all))
                .set_text(cx, &format!("共: {}张", self.state.image_paths.len()));
            let query = self.ui.text_input(cx, ids!(query)).text();
            self.filter_image_paths(cx, &query);
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
        if let Some(image_idx) = self.state.current_image_idx {
            if image_idx > 0 {
                self.set_current_image(cx, Some(image_idx - 1));
            }
        }
    }

    fn navigate_right(&mut self, cx: &mut Cx) {
        if let Some(image_idx) = self.state.current_image_idx {
            if image_idx + 1 < self.state.num_images() {
                self.set_current_image(cx, Some(image_idx + 1));
            }
        }
    }

    fn set_current_image(&mut self, cx: &mut Cx, image_idx: Option<usize>) {
        self.state.current_image_idx = image_idx;
        self.state.image_zoom = 1.0;
        self.state.image_pan = dvec2(0.0, 0.0);
        let image = self.ui.image(cx, ids!(slideshow.image_view.image));
        if let Some(image_idx) = self.state.current_image_idx {
            let filtered_image_idx = self.state.filtered_image_idxs[image_idx];
            let image_path = &self.state.image_paths[filtered_image_idx];
            let _ = image.load_image_file_by_path_async(cx, &image_path);
        } else {
            let _ = image.load_image_dep_by_path(cx, "");
        }
        if self.state.in_slideshow {
            self.apply_image_zoom(cx);
        }
        self.ui.redraw(cx);
    }

    fn apply_image_zoom(&mut self, cx: &mut Cx) {
        let zoom = self.state.image_zoom;
        ZOOM_FACTOR.store(f64::to_bits(zoom), Ordering::Relaxed);
        ZOOM_PAN_X.store(f64::to_bits(self.state.image_pan.x), Ordering::Relaxed);
        ZOOM_PAN_Y.store(f64::to_bits(self.state.image_pan.y), Ordering::Relaxed);
        self.ui.redraw(cx);
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
        if let Event::Scroll(se) = event {
            if self.state.in_slideshow && se.is_mouse && se.scroll.y != 0.0 {
                let old_zoom = self.state.image_zoom;
                let new_zoom = if se.scroll.y > 0.0 {
                    old_zoom / 1.1
                } else {
                    old_zoom * 1.1
                };
                let new_zoom = new_zoom.clamp(0.1, 8.0);
                if (new_zoom - old_zoom).abs() > 0.00001 {
                    let zoom_ref = self.ui.widget(cx, ids!(slideshow.image_view));
                    if let Some(zv) = zoom_ref.borrow_mut::<ZoomableView>() {
                        let viewport = zv.last_rect;
                        if viewport.contains(se.abs) {
                            // 光标锚定缩放：保持鼠标指向的图像点不动
                            let k = new_zoom / old_zoom;
                            let rs = viewport.size;
                            let cursor = se.abs - viewport.pos;
                            let scaled = dvec2(rs.x * old_zoom, rs.y * old_zoom);
                            let top_left = self.state.image_pan + (rs - scaled) * 0.5;
                            let image_pos = cursor - top_left;
                            let scaled_new = dvec2(rs.x * new_zoom, rs.y * new_zoom);
                            let top_left_new = cursor - image_pos * k;
                            self.state.image_pan = top_left_new - (rs - scaled_new) * 0.5;
                        } else {
                            // 光标在视口外：退回居中缩放
                            self.state.image_pan = dvec2(0.0, 0.0);
                        }
                    }
                    if (new_zoom - 1.0).abs() < 0.001 {
                        self.state.image_pan = dvec2(0.0, 0.0);
                    }
                    self.state.image_zoom = new_zoom;
                    self.apply_image_zoom(cx);
                }
            }
        }
        if matches!(event, Event::Draw(_)) {
            if DBG_DRAW_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                DBG_TIMER_ID.store(cx.start_timeout(3.0).0, Ordering::Relaxed);
            }
        }
        if let Event::Timer(te) = event {
            if te.timer_id == DBG_TIMER_ID.load(Ordering::Relaxed) && !DBG_CAP1.swap(true, Ordering::Relaxed)
            {
                cx.capture_next_frame_to_file(std::path::PathBuf::from(
                    "C:/Users/Administrator/AppData/Local/Temp/opencode/cap_app.png",
                ));
            }
        }
        if matches!(event, Event::Draw(_)) {
            if let Some(sp) = self.ui.splitter(cx, ids!(split_view)).borrow() {
                dbg(&format!(
                    "SPLITTER align={:?} axis={:?} pos={:.0} area_a={:?} area_b={:?}",
                    sp.align(),
                    sp.axis(),
                    sp.position(),
                    sp.area_a().rect(cx),
                    sp.area_b().rect(cx)
                ));
            } else {
                dbg("SPLITTER not found");
            }
        }
        if matches!(event, Event::Draw(_)) && self.state.image_paths.is_empty() {
            let path = std::path::PathBuf::from("C:/Windows/Web/Wallpaper");
            self.state.loaded_image_idxs.clear();
            let (image_paths, root) = build_image_tree(&path);
            self.state.image_paths = image_paths;
            self.state.file_tree_data = Some(FileTreeData {
                dir_path: path,
                root,
            });
            let mut splitter = self.ui.splitter(cx, ids!(split_view));
            splitter.set_align(cx, SplitterAlign::FromA(200.0));
            self.ui
                .label(cx, ids!(all))
                .set_text(cx, &format!("共: {}张", self.state.image_paths.len()));
            self.filter_image_paths(cx, "");
        }
        let mut scope = Scope::with_data(&mut self.state);
        self.ui.handle_event(cx, event, &mut scope);
        // 移除无条件重绘——异步图片加载完毕后会自行触发绘制，Scroll/Drag 也会自行驱动
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(query) = self.ui.text_input(cx, ids!(query)).changed(&actions) {
            self.filter_image_paths(cx, &query);
        }
        if self.ui.button(cx, ids!(search_btn)).clicked(&actions) {
            self.load_image_paths(cx);
        }
        if self.ui.button(cx, ids!(slideshow_button)).clicked(&actions) {
            self.open_slideshow(cx);
        }
        // 文件树展开/收起按钮
        if self.ui.button(cx, ids!(filetree_btn)).clicked(&actions) {
            if self.state.file_tree_data.is_some() {
                self.state.file_tree_open = !self.state.file_tree_open;
                let align = if self.state.file_tree_open {
                    SplitterAlign::FromA(200.0)
                } else {
                    SplitterAlign::FromA(0.0)
                };
                // 直接设置 Splitter 的 align 属性并 redraw
                let mut splitter = self.ui.splitter(cx, ids!(split_view));
                splitter.set_align(cx, align);
                self.ui.redraw(cx);
                self.ui.redraw(cx);
            }
        }
        // FileTree 条目点击 + 图片卡片点击 - 用 action 遍历
        for action in actions.iter() {
            if let Some(wa) = action.downcast_ref::<WidgetAction>() {
                if let Some(ftc) = wa.action.downcast_ref::<FileTreeClicked>() {
                    let target_idx = ftc.image_index;
                    if let Some(pos) = self.state.filtered_image_idxs.iter().position(|&i| i == target_idx) {
                        self.set_current_image(cx, Some(pos));
                    } else {
                        self.state.current_image_idx = Some(target_idx);
                        let image_path = &self.state.image_paths[target_idx];
                        let image = self.ui.image(cx, ids!(slideshow.image_view.image));
                        let _ = image.load_image_file_by_path_async(cx, image_path);
                    }
                    self.open_slideshow(cx);
                }
                if let Some(clicked) = wa.action.downcast_ref::<ImageClicked>() {
                    self.set_current_image(cx, Some(clicked.image_index));
                    self.open_slideshow(cx);
                }
            }
        }
        // 幻灯片左右导航按钮
        if self.ui.button(cx, ids!(navigate_left)).clicked(&actions) {
            self.navigate_left(cx);
        }
        if self.ui.button(cx, ids!(navigate_right)).clicked(&actions) {
            self.navigate_right(cx);
        }
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
#[derive(Debug)]
pub struct State {
    image_paths: Vec<PathBuf>,
    filtered_image_idxs: Vec<usize>,
    max_images_per_row: usize,
    current_image_idx: Option<usize>,
    in_slideshow: bool,
    image_zoom: f64,
    image_pan: Vec2d,
    file_tree_data: Option<FileTreeData>,
    file_tree_open: bool,
    loaded_image_idxs: HashSet<usize>,
}

impl State {
    fn num_images(&self) -> usize {
        self.filtered_image_idxs.len()
    }

    fn num_rows(&self) -> usize {
        self.num_images().div_ceil(self.max_images_per_row)
    }

    fn first_image_idx_for_row(&self, row_idx: usize) -> usize {
        row_idx * self.max_images_per_row
    }

    fn num_images_for_row(&self, row_idx: usize) -> usize {
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
            image_zoom: 1.0,
            image_pan: dvec2(0.0, 0.0),
            file_tree_data: None,
            file_tree_open: false,
            loaded_image_idxs: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageClicked {
    pub image_index: usize,
}

#[derive(Clone, Debug)]
pub struct FileTreeClicked {
    pub image_index: usize,
}

// 文件树节点（文件夹 image_idx=None 且有 children，图片文件 image_idx=Some(索引)）
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
fn build_image_tree(dir_path: &Path) -> (Vec<PathBuf>, FileTreeNode) {
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

// ---------------------------------------------------------------------------
// ImageCardWidget：包裹固定尺寸卡片 View 的自定义 Widget
//   hover 动画：由 View 的 Animator 原生处理（PortalList → handle_event → View::hits → Animator）
//   点击：由 Widget::handle_event 手动检测 FingerUp → ImageClicked
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct ImageCardWidget {
    #[deref]
    view: View,
    #[live]
    draw_bg: DrawColor,
    #[rust]
    image_index: usize,
    #[rust]
    is_hovering: bool,
}

impl Widget for ImageCardWidget {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.peek_walk_turtle(walk);

        if self.is_hovering {
            // highlight behind
            let pad = 10.0;
            self.draw_bg.color = vec4(0.45, 0.60, 0.85, 0.20);
            self.draw_bg.draw_abs(
                cx,
                Rect {
                    pos: dvec2(rect.pos.x - pad, rect.pos.y - pad),
                    size: dvec2(rect.size.x + pad * 2.0, rect.size.y + pad * 2.0),
                },
            );

            // advance main turtle by normal walk (keep layout correct)
            cx.walk_turtle(walk);

            // draw zoomed card in isolated turtle — won't affect sibling positions
            let scale = 1.12;
            let sw = rect.size.x * scale;
            let sh = rect.size.y * scale;
            let dx = (rect.size.x - sw) * 0.5;
            let dy = (rect.size.y - sh) * 0.5;

            cx.begin_turtle(
                Walk {
                    width: Size::Fixed(sw),
                    height: Size::Fixed(sh),
                    abs_pos: Some(dvec2(rect.pos.x + dx, rect.pos.y + dy)),
                    margin: Inset::default(),
                    metrics: Default::default(),
                },
                Layout::default(),
            );
            let _ = self.view.draw_walk(
                cx,
                scope,
                Walk {
                    width: Size::Fixed(sw),
                    height: Size::Fixed(sh),
                    abs_pos: None,
                    margin: Inset::default(),
                    metrics: Default::default(),
                },
            );
            cx.end_turtle();

            DrawStep::done()
        } else {
            self.view.draw_walk(cx, scope, walk)
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let area = self.view.area();
        let hit = event.hits(cx, area);

        match hit {
            Hit::FingerHoverIn(_) => {
                if !self.is_hovering {
                    self.is_hovering = true;
                    self.view.animator_play(cx, ids!(hover.on));
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.is_hovering {
                    self.is_hovering = false;
                    self.view.animator_play(cx, ids!(hover.off));
                    self.redraw(cx);
                }
            }
            Hit::FingerUp(fe) => {
                if fe.is_over && fe.was_tap() && fe.is_primary_hit() {
                    cx.widget_action(
                        self.widget_uid(),
                        ImageClicked {
                            image_index: self.image_index,
                        },
                    );
                }
            }
            _ => {}
        }
        self.view.handle_event(cx, event, scope);
    }
}

// ---------------------------------------------------------------------------
// ImageGrid：外层 PortalList（flow: Down），每行一个 ImageRow
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct ImageGrid {
    #[deref]
    view: View,
}

impl Widget for ImageGrid {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        dbg("GRID draw_walk called");
        let rect = cx.peek_walk_turtle(walk);
        dbg(&format!(
            "GRID rect pos=({:.0},{:.0}) size={:.0}x{:.0}",
            rect.pos.x, rect.pos.y, rect.size.x, rect.size.y
        ));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let state = scope.data.get_mut::<State>().unwrap();
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let grid_width = cx.peek_walk_turtle(walk).size.x.max(1.0);
                let spacing = 12.0;
                let card_width = 250.0;
                let cols = ((grid_width + spacing) / (card_width + spacing))
                    .floor()
                    .max(1.0) as usize;
                state.max_images_per_row = cols;
                list.set_item_range(cx, 0, state.num_rows());
                dbg(&format!(
                    "GRID width={:.0} cols={} rows={}",
                    grid_width,
                    cols,
                    state.num_rows()
                ));
                while let Some(row_idx) = list.next_visible_item(cx) {
                    if row_idx >= state.num_rows() {
                        continue;
                    }
                    let row = list.item(cx, row_idx, live_id!(ImageRow));
                    let mut row_scope = Scope::with_data_props(state, &row_idx);
                    row.draw_all(cx, &mut row_scope);
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope)
    }
}

// ---------------------------------------------------------------------------
// ImageRow：内层 PortalList（flow: Right），每项是 ImageCardWidget
//   设置 card.image_index + card.image_path，然后 draw_all
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct ImageRow {
    #[deref]
    view: View,
}

impl Widget for ImageRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // 先提取需要的值，避免和 self.view.draw_walk 的 scope 借冲突
        let (num_items, first_idx) = {
            let state = scope.data.get_mut::<State>().unwrap();
            let row_idx = *scope.props.get::<usize>().unwrap();
            (
                state.num_images_for_row(row_idx),
                state.first_image_idx_for_row(row_idx),
            )
        };

        let rect = cx.peek_walk_turtle(walk);
        let card_width = 250.0_f64;
        let spacing = 12.0_f64;
        let row_content_width =
            num_items as f64 * card_width + (num_items.saturating_sub(1)) as f64 * spacing;
        let margin_left = ((rect.size.x - row_content_width) * 0.5).max(0.0);

        let row_walk = Walk {
            margin: Inset {
                left: margin_left,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
            ..walk
        };

        while let Some(item) = self.view.draw_walk(cx, scope, row_walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, num_items);
                while let Some(item_idx) = list.next_visible_item(cx) {
                    if item_idx >= num_items {
                        continue;
                    }
                    let image_idx = first_idx + item_idx;

                    // 仅对尚未请求加载的图片触发异步加载
                    let (need_load, image_path) = {
                        let state = scope.data.get_mut::<State>().unwrap();
                        let filtered_image_idx = state.filtered_image_idxs[image_idx];
                        if state.loaded_image_idxs.insert(filtered_image_idx) {
                            (true, state.image_paths[filtered_image_idx].clone())
                        } else {
                            (false, PathBuf::new())
                        }
                    };

                    let card = list.item(cx, item_idx, live_id!(ImageCard));
                    if let Some(mut card) = card.borrow_mut::<ImageCardWidget>() {
                        card.image_index = image_idx;
                    }
                    if need_load {
                        let _ = card.image(cx, ids!(thumb))
                            .load_image_file_by_path_async(cx.cx, &image_path);
                    }
                    card.draw_all(cx, &mut Scope::empty());
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope)
    }
}

// ---------------------------------------------------------------------------
// ZoomableView：居中缩放 + 光标锚定缩放（App 的 Scroll 处理里更新 pan）
//   last_rect 记录上一帧视口矩形，供 Scroll 事件计算锚点
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct ZoomableView {
    #[deref]
    view: View,
    #[rust]
    pub last_rect: Rect,
}

impl Widget for ZoomableView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.peek_walk_turtle(walk);
        self.last_rect = rect;
        let zoom = f64::from_bits(ZOOM_FACTOR.load(Ordering::Relaxed));
        let pan = dvec2(
            f64::from_bits(ZOOM_PAN_X.load(Ordering::Relaxed)),
            f64::from_bits(ZOOM_PAN_Y.load(Ordering::Relaxed)),
        );
        if (zoom - 1.0).abs() < 0.001 && pan.length() < 0.001 {
            return self.view.draw_walk(cx, scope, walk);
        }
        // 以视口中心为基准 + pan 偏移作为图像左上角（abs_pos 为绝对位置）
        let zw = rect.size.x * zoom;
        let zh = rect.size.y * zoom;
        self.view.draw_walk(cx, scope, Walk {
            width: Size::Fixed(zw),
            height: Size::Fixed(zh),
            abs_pos: Some(rect.pos + dvec2(
                (rect.size.x - zw) * 0.5 + pan.x,
                (rect.size.y - zh) * 0.5 + pan.y,
            )),
            margin: Inset::default(),
            metrics: Default::default(),
        })
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope)
    }
}

// ---------------------------------------------------------------------------
// FileTreePanel：文件树组件（参照 DemoFileTree 模式）
//   #[live] pub file_tree: FileTree 直接绑定
//   draw_walk 用 while .is_step() 两步法在树之间填充节点
//   节点 id：根 = live_id!(root)，文件 = LiveId(i+1)，点击经 WidgetMatchEvent 发射 FileTreeClicked
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct FileTreePanel {
    #[uid]
    uid: WidgetUid,
    #[redraw]
    #[live]
    pub file_tree: FileTree,
}
impl Widget for FileTreePanel {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        dbg("FILETREE draw_walk called");
        let rect = cx.peek_walk_turtle(walk);
        dbg(&format!(
            "FILETREE rect pos=({:.0},{:.0}) size={:.0}x{:.0}",
            rect.pos.x, rect.pos.y, rect.size.x, rect.size.y
        ));
        while self.file_tree.draw_walk(cx, scope, walk).is_step() {
            self.file_tree
                .set_folder_is_open(cx, live_id!(root).into(), true, Animate::No);
            if let Some(state) = scope.data.get::<State>() {
                if let Some(ref ft_data) = state.file_tree_data {
                    Self::draw_tree_node(cx, &mut self.file_tree, &ft_data.root, live_id!(root).into());
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.file_tree.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }
}

impl FileTreePanel {
    // 递归绘制文件树节点：图片文件用 image_idx+1 做节点 id，文件夹用路径哈希做节点 id
    // 首次出现的文件夹自动展开（is_folder 尚不存在时），之后保持用户的开合选择
    fn draw_tree_node(cx: &mut Cx2d, file_tree: &mut FileTree, node: &FileTreeNode, node_id: LiveId) {
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
impl WidgetMatchEvent for FileTreePanel {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        let ft_uid = self.file_tree.widget_uid();
        for action in actions {
            if let Some(item) = action.as_widget_action() {
                if item.widget_uid == ft_uid {
                    if let FileTreeAction::FileClicked(file_id) = item.cast() {
                        // 文件节点 id = image_idx + 1，直接对应 image_paths 下标
                        let idx = file_id.0 as usize - 1;
                        let state = _scope.data.get::<State>().unwrap();
                        if idx < state.image_paths.len() {
                            cx.widget_action(
                                self.widget_uid(),
                                FileTreeClicked {
                                    image_index: idx,
                                },
                            );
                        }
                    }
                }
            }
        }
    }
}

app_main!(App);
