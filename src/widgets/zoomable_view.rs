use makepad_widgets::*;
use crate::state::State;

// ---------------------------------------------------------------------------
// ZoomableView：居中缩放 + 光标锚定缩放 + 拖拽平移。
//   借鉴 Robrix ImageViewer：缩放/平移状态收拢在本组件内部（DragState），
//   不再依赖全局 static，缩放计算收敛为 apply_frame()。
// ---------------------------------------------------------------------------

/// 缩放 / 平移配置（可调，避免魔法数字）。
#[derive(Clone, Debug)]
pub struct ZoomConfig {
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub zoom_scale_factor: f64,
    pub pan_sensitivity: f64,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            min_zoom: 0.1,
            max_zoom: 8.0,
            zoom_scale_factor: 1.1,
            pan_sensitivity: 1.0,
        }
    }
}

/// 拖拽 / 缩放状态，作为 widget 的 `#[rust]` 字段，随 widget 生命周期走。
#[derive(Clone, Debug)]
struct DragState {
    /// 当前缩放倍数（1.0 = 原始大小）。
    zoom_level: f64,
    /// 平移偏移（屏幕坐标）。
    pan_offset: DVec2,
    /// 拖拽起始位置与起始平移，用于增量平移。
    drag_start: DVec2,
    pan_start: DVec2,
    dragging: bool,
    /// 双指捏合时，上一次两指之间的距离（用于计算缩放比例）。
    previous_pinch_distance: Option<f64>,
}

impl Default for DragState {
    fn default() -> Self {
        Self {
            zoom_level: 1.0, // 关键：初始缩放必须是 1.0（原始大小），否则图片尺寸为 0
            pan_offset: dvec2(0.0, 0.0),
            drag_start: dvec2(0.0, 0.0),
            pan_start: dvec2(0.0, 0.0),
            dragging: false,
            previous_pinch_distance: None,
        }
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

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
}

#[derive(Script, ScriptHook, Widget)]
pub struct ZoomableView {
    #[deref]
    view: View,
    #[rust]
    drag_state: DragState,
    #[rust]
    config: ZoomConfig,
    /// 最近一次布局区域，供 hit-test 与缩放锚点计算使用。
    #[rust]
    last_rect: Rect,
}

impl Widget for ZoomableView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.peek_walk_turtle(walk);
        self.last_rect = rect;

        let zoom = self.drag_state.zoom_level;
        let pan = self.drag_state.pan_offset;

        // 未缩放且未平移时，走默认布局，避免多余的开销。
        if (zoom - 1.0).abs() < 0.001 && pan.length() < 0.001 {
            return self.view.draw_walk(cx, scope, walk);
        }

        let zw = rect.size.x * zoom;
        let zh = rect.size.y * zoom;
        let start = cx.align_list_len();
        self.view.draw_walk(
            cx,
            scope,
            Walk {
                width: Size::Fixed(zw),
                height: Size::Fixed(zh),
                abs_pos: Some(rect.pos + dvec2((rect.size.x - zw) * 0.5, (rect.size.y - zh) * 0.5)),
                margin: Inset::default(),
                metrics: Default::default(),
            },
        )?;
        cx.shift_align_range(
            &TurtleAlignRange {
                start,
                end: cx.align_list_len(),
            },
            pan,
        );
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let in_slideshow = scope.data.get::<State>().map(|s| s.in_slideshow).unwrap_or(false);

        // 键盘缩放（+ / - / 0），桌面端与移动端外接键盘均适用。
        if let Event::KeyDown(ke) = event {
            if in_slideshow {
                match ke.key_code {
                    KeyCode::Equals | KeyCode::NumpadAdd => {
                        self.adjust_zoom(cx, 1.0 + 0.02);
                        return;
                    }
                    KeyCode::Minus | KeyCode::NumpadSubtract => {
                        self.adjust_zoom(cx, 1.0 / (1.0 + 0.02));
                        return;
                    }
                    KeyCode::Key0 | KeyCode::Numpad0 => {
                        self.reset_zoom(cx);
                        return;
                    }
                    _ => {}
                }
            }
        }

        // 使用 hit-test 统一处理桌面鼠标与移动端触屏（均映射为 Finger* 事件）。
        let area = self.view.area();
        match event.hits(cx, area) {
            Hit::FingerDown(fe) if in_slideshow && self.drag_state.zoom_level > 1.0 => {
                // 双击重置缩放/平移。
                if fe.tap_count >= 2 {
                    self.reset_zoom(cx);
                    self.drag_state.dragging = false;
                } else if fe.is_primary_hit() {
                    self.drag_state.dragging = true;
                    self.drag_state.drag_start = fe.abs;
                    self.drag_state.pan_start = self.drag_state.pan_offset;
                }
            }
            Hit::FingerMove(fe) if in_slideshow && self.drag_state.dragging => {
                let delta = (fe.abs - self.drag_state.drag_start) * self.config.pan_sensitivity;
                self.drag_state.pan_offset = self.drag_state.pan_start + delta;
                self.redraw(cx);
            }
            Hit::FingerUp(fe) if in_slideshow && self.drag_state.dragging => {
                if fe.is_over {
                    self.drag_state.dragging = false;
                }
            }
            Hit::FingerScroll(fse) if in_slideshow => {
                // 精细化滚轮缩放：滚动幅度越大缩放越快（借鉴 Robrix）。
                let scroll_delta = fse.scroll.y;
                if scroll_delta != 0.0 {
                    let normalized = (scroll_delta.abs() / 200.0).clamp(0.005, 0.06);
                    let factor = if scroll_delta > 0.0 {
                        1.0 / (1.0 + normalized)
                    } else {
                        1.0 + normalized
                    };
                    if self.last_rect.contains(fse.abs) {
                        self.anchor_zoom(cx, factor, fse.abs);
                    } else {
                        self.adjust_zoom(cx, factor);
                    }
                }
            }
            _ => {}
        }

        // 双指捏合缩放（触屏多点手势）。
        if let Event::TouchUpdate(touch_event) = event {
            if in_slideshow {
                if touch_event.touches.len() == 2 {
                    let touch1 = &touch_event.touches[0];
                    let touch2 = &touch_event.touches[1];
                    let current_distance = (touch1.abs - touch2.abs).length();

                    if let Some(previous_distance) = self.drag_state.previous_pinch_distance {
                        let scale = current_distance / previous_distance;
                        self.adjust_zoom(cx, scale);
                    }
                    self.drag_state.previous_pinch_distance = Some(current_distance);
                } else {
                    self.drag_state.previous_pinch_distance = None;
                }
            }
        }

        self.view.handle_event(cx, event, scope);
    }
}

impl ZoomableView {
    /// 重置缩放与平移（切图时由 App 调用）。
    pub fn reset_zoom(&mut self, cx: &mut Cx) {
        self.drag_state = DragState::default();
        self.redraw(cx);
    }

    /// 以 `factor` 为乘数调整缩放，并保持中心锚点。
    fn adjust_zoom(&mut self, cx: &mut Cx, factor: f64) {
        let new_zoom = (self.drag_state.zoom_level * factor)
            .clamp(self.config.min_zoom, self.config.max_zoom);
        if (new_zoom - self.drag_state.zoom_level).abs() < 0.00001 {
            return;
        }
        self.drag_state.zoom_level = new_zoom;
        if (new_zoom - 1.0).abs() < 0.001 {
            self.drag_state.pan_offset = dvec2(0.0, 0.0);
        }
        self.redraw(cx);
    }

    /// 光标锚定缩放：缩放时保持 `anchor`（屏幕坐标）下的图像点不动。
    fn anchor_zoom(&mut self, cx: &mut Cx, factor: f64, anchor: DVec2) {
        let old_zoom = self.drag_state.zoom_level;
        let new_zoom = (old_zoom * factor).clamp(self.config.min_zoom, self.config.max_zoom);
        if (new_zoom - old_zoom).abs() < 0.00001 {
            return;
        }

        let k = new_zoom / old_zoom;
        let rs = self.last_rect.size;
        let cursor = anchor - self.last_rect.pos;
        // 当前缩放后的图像左上角（屏幕坐标）。
        let scaled = dvec2(rs.x * old_zoom, rs.y * old_zoom);
        let top_left = self.drag_state.pan_offset + (rs - scaled) * 0.5;
        // 光标在图像坐标系中的位置。
        let image_pos = cursor - top_left;
        let scaled_new = dvec2(rs.x * new_zoom, rs.y * new_zoom);
        let top_left_new = cursor - image_pos * k;
        self.drag_state.pan_offset = top_left_new - (rs - scaled_new) * 0.5;
        self.drag_state.zoom_level = new_zoom;

        if (new_zoom - 1.0).abs() < 0.001 {
            self.drag_state.pan_offset = dvec2(0.0, 0.0);
        }
        self.redraw(cx);
    }
}
