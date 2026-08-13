use makepad_widgets::*;
use crate::actions::{AppCommand, ThumbDecoded};
use crate::state::State;
use crate::thumb::{downscale_image, THUMB_MAX_DIM, THUMB_MAX_IN_FLIGHT};
use crate::tree::{build_image_tree, FileTreeData};
use crate::widgets::file_tree_panel::FileTreePanel;
use crate::widgets::image_grid::ImageGrid;
use crate::widgets::zoomable_view::ZoomableView;

// 顶层 DSL，包含全部页面与组件布局
live_design! {
    use link::theme::*;
    use link::widgets::*;
    use link::shaders::*;

    use crate::widgets::file_tree_panel::FileTreePanel;
    use crate::widgets::image_grid::ImageGrid;
    use crate::widgets::image_card::ImageCard;
    use crate::widgets::zoomable_view::ZoomableView;

    // 根布局：顶部工具栏 + 下方内容（文件树 / 网格） + 右侧幻灯片视口
    pub App = {{App}} {
        ui: <Root> {
            root: <View> {
                width: Fill, height: Fill,
                flow: Down,
                padding: 8,
                spacing: 8,
                // 顶部工具栏
                toolbar: <View> {
                    width: Fill, height: 40,
                    flow: Right,
                    spacing: 8,
                    align: { y: 0.5 },
                    query_input: <TextInput> {
                        width: 300, height: 30,
                        empty_message: "搜索图片关键字...",
                        draw_text: { text_style: { font_size: 12 } },
                        draw_border: { border_color: #4 },
                    }
                    search_btn: <Button> {
                        width: 80, height: 30, text: "加载",
                        draw_text: { text_style: { font_size: 12 } },
                    }
                    slideshow_button: <Button> {
                        width: 80, height: 30, text: "幻灯片",
                        draw_text: { text_style: { font_size: 12 } },
                    }
                }
                // 主内容区：左侧文件树 + 右侧（上方网格 / 幻灯片视口，用 PageFlip 切换）
                split_view: <Splitter> {
                    width: Fill, height: Fill,
                    align: FromA(0.0),
                    axis: Horizontal,
                    a: <View> {
                        width: Fill, height: Fill,
                        flow: Down, spacing: 4,
                        body: <View> {
                            width: Fill, height: Fill,
                            flow: Down, spacing: 4,
                            file_tree_panel: <FileTreePanel> {},
                            grid_pager: <Pager> {
                               .Pager = { flow: Down, spacing: 8, align: {x: 0.5} }
                                grid_scroll: <View> {
                                    width: Fill, height: Fill, flow: Down, spacing: 8,
                                    grid: <ImageGrid> {}
                                }
                            }
                        }
                    }
                    b: <View> {
                        width: Fill, height: Fill,
                        flow: Down, spacing: 4,
                        slideshow: <View> {
                            width: Fill, height: Fill, flow: Down, spacing: 4,
                            top_bar: <View> {
                                width: Fill, height: 40, flow: Right, spacing: 8, align: { y: 0.5 },
                                current_label: <Label> {
                                    width: Fill, height: Fill, align: { y: 0.5 },
                                    draw_text: { text_style: { font_size: 14 }, color: #E },
                                    text: "请在左侧选择一个图片",
                                }
                                navigate_left: <Button> { width: 60, height: 30, text: "上一张" }
                                navigate_right: <Button> { width: 60, height: 30, text: "下一张" }
                            }
                            image_view: <ZoomableView> {}
                        }
                    }
                }
            }
        }
    }
}

// 后台解码线程任务
pub fn start_decode_thread(cx: &mut Cx, paths: Vec<PathBuf>, from_idx: usize, to_idx: usize) {
    let sender = cx.sender::<ThumbDecoded>();
    std::thread::spawn(move || {
        for image_idx in from_idx..to_idx {
            if image_idx >= paths.len() {
                break;
            }
            let path = &paths[image_idx];
            if let Ok(buf) = cx.read_image_buffer_sync(path) {
                if let Some((w, h, data)) = downscale_image(&buf, THUMB_MAX_DIM) {
                    let _ = sender.send(ThumbDecoded {
                        image_idx,
                        width: w,
                        height: h,
                        data,
                    });
                }
            }
        }
    });
}

// 加载文件夹 action：由 App 通过 Cx::post_action 发出，在事件循环中被消费
#[derive(Clone, Debug)]
pub struct LoadFolder(pub PathBuf);

pub struct App {
    #[rust] state: State,
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // 主循环：统一分发 event 与 action。各组件在自己的 handle_event /
        // WidgetMatchEvent::handle_actions 中处理自身逻辑；App 只做协调。
        self.match_event(cx, event);
        if let Event::KeyDown(ke) = event {
            if ke.key_code == KeyCode::Escape {
                if self.state.in_slideshow {
                    self.state.in_slideshow = false;
                    self.ui.page_flip(id!(split_view.body.grid_pager)).set_page(cx, 0, 0);
                } else {
                    self.state.file_tree_open = false;
                    let splitter = self.ui.splitter(cx, ids!(split_view));
                    splitter.set_align(cx, SplitterAlign::FromA(0.0));
                }
                self.ui.redraw(cx);
            }
            if ke.key_code == KeyCode::ArrowLeft && self.state.in_slideshow {
                self.navigate_left(cx);
            }
            if ke.key_code == KeyCode::ArrowRight && self.state.in_slideshow {
                self.navigate_right(cx);
            }
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // 1) 按钮等 App 壳控件（无独立组件）直接响应
        if let Some(query) = self.ui.text_input(cx, ids!(query)).changed(actions) {
            self.filter_image_paths(cx, &query);
        }
        if self.ui.button(cx, ids!(search_btn)).clicked(actions) {
            self.load_image_paths(cx);
        }
        if self.ui.button(cx, ids!(slideshow_button)).clicked(actions) {
            self.open_slideshow(cx);
        }
        if self
            .ui
            .button(cx, ids!(filetree_btn))
            .clicked(actions)
            && self.state.file_tree_data.is_some()
        {
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

        // 2) 组件发来的高层协调命令（打开图片）
        for action in actions.iter() {
            if let Some(cmd) = action.downcast_ref::<AppCommand>() {
                if let AppCommand::OpenImage(target_idx) = cmd {
                    self.open_image(*target_idx, cx);
                }
            }
        }
    }
}

impl MatchEvent for App {
    fn match_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Some(LoadFolder(dir)) = Cx::take_action() {
            self.load_folder(cx, dir);
            return;
        }
        let mut scope = Scope::with_data(&mut self.state);
        self.ui.handle_event(cx, event, &mut scope);
        self.ui.match_event(cx, event, &mut scope);
    }
}

impl App {
    fn load_folder(&mut self, cx: &mut Cx, dir: PathBuf) {
        let (image_paths, root) = build_image_tree(&dir);
        self.state.image_paths = image_paths.clone();
        self.state.filtered_image_idxs = (0..image_paths.len()).collect();
        self.state.file_tree_data = Some(FileTreeData {
            dir_path: dir.clone(),
            root: root.clone(),
        });
        self.state.thumb_in_flight.clear();
        for &idx in &self.state.filtered_image_idxs {
            self.state.thumb_in_flight.insert(idx);
        }
        let in_flight: Vec<_> = self.state.thumb_in_flight.iter().copied().collect();
        let chunks: Vec<(usize, usize)> = in_flight
            .chunks(THUMB_MAX_IN_FLIGHT)
            .map(|c| (c[0], c[c.len() - 1] + 1))
            .collect();
        for (from, to) in chunks {
            start_decode_thread(cx, image_paths.clone(), from, to);
        }
        self.ui.redraw(cx);
    }

    fn open_image(&mut self, target_idx: usize, cx: &mut Cx) {
        if let Some(pos) = self
            .state
            .filtered_image_idxs
            .iter()
            .position(|&i| i == target_idx)
        {
            self.set_current_image(cx, Some(pos));
        } else {
            self.state.current_image_idx = Some(target_idx);
            let image_path = &self.state.image_paths[target_idx];
            let image = self.ui.image(cx, ids!(slideshow.image_view.image));
            let _ = image.load_image_file_by_path_async(cx, image_path);
        }
        self.open_slideshow(cx);
    }

    fn load_image_paths(&mut self, cx: &mut Cx) {
        let folder = rfd::FileDialog::new().pick_folder();
        if let Some(dir) = folder {
            Cx::post_action(LoadFolder(dir));
        } else {
            println!("未选择文件夹");
        }
        self.ui.redraw(cx);
    }

    fn filter_image_paths(&mut self, cx: &mut Cx, query: &str) {
        let q = query.trim().to_lowercase();
        self.state.filtered_image_idxs.clear();
        for (i, p) in self.state.image_paths.iter().enumerate() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if q.is_empty() || name.to_lowercase().contains(&q) {
                self.state.filtered_image_idxs.push(i);
            }
        }
        self.state.current_image_idx = None;
        self.ui.redraw(cx);
    }

    fn set_current_image(&mut self, cx: &mut Cx, pos: Option<usize>) {
        self.state.current_image_idx = pos.map(|p| self.state.filtered_image_idxs[p]);
        let p = self.state.current_image_idx;
        if let Some(idx) = p {
            self.state.image_zoom = 1.0;
            self.state.image_pan = dvec2(0.0, 0.0);
            self.state.image_dragging = false;
            crate::thumb::store_zoom_state(1.0, dvec2(0.0, 0.0));
            let image_path = &self.state.image_paths[idx];
            let image = self.ui.image(cx, ids!(slideshow.image_view.image));
            let _ = image.load_image_file_by_path_async(cx, image_path);
            let label = self.ui.label(cx, ids!(slideshow.current_label));
            label.set_text(&format!("{}", image_path.display()));
        } else {
            let label = self.ui.label(cx, ids!(slideshow.current_label));
            label.set_text("请在左侧选择一个图片");
        }
        self.ui.redraw(cx);
    }

    fn open_slideshow(&mut self, cx: &mut Cx) {
        self.state.in_slideshow = true;
        self.ui
            .page_flip(id!(split_view.body.grid_pager))
            .set_page(cx, 0, 1);
        self.ui.redraw(cx);
    }

    fn navigate_left(&mut self, cx: &mut Cx) {
        let cur = self.state.current_image_idx;
        if let Some(c) = cur {
            if c > 0 {
                self.set_current_image(cx, Some(c - 1));
            }
        }
    }

    fn navigate_right(&mut self, cx: &mut Cx) {
        let cur = self.state.current_image_idx;
        if let Some(c) = cur {
            if c + 1 < self.state.image_paths.len() {
                self.set_current_image(cx, Some(c + 1));
            }
        }
    }
}
