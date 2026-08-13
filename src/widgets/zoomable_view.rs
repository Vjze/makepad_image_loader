use std::sync::atomic::Ordering;

use makepad_widgets::*;
use crate::state::State;
use crate::thumb::{store_zoom_state, ZOOM_FACTOR, ZOOM_PAN_X, ZOOM_PAN_Y};

// ---------------------------------------------------------------------------
// ZoomableView：居中缩放 + 光标锚定缩放 + 拖拽平移
//   缩放/拖拽交互下沉到本组件自身的 handle_event（通过 scope 读写 State）
// ---------------------------------------------------------------------------
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
        // 缩放 / 拖拽交互由本组件自己处理
        let state = scope.data.get_mut::<State>().unwrap();
        match event {
            Event::MouseDown(mouse)
                if mouse.button == MouseButton::PRIMARY
                    && state.in_slideshow
                    && state.image_zoom > 1.0
                    && self.last_rect.contains(mouse.abs) => {
                state.image_dragging = true;
                state.image_drag_start = mouse.abs;
                state.image_pan_start = state.image_pan;
            }
            Event::MouseMove(mouse)
                if state.in_slideshow && state.image_dragging => {
                state.image_pan =
                    state.image_pan_start + (mouse.abs - state.image_drag_start);
                store_zoom_state(state.image_zoom, state.image_pan);
                self.redraw(cx);
            }
            Event::MouseUp(mouse)
                if mouse.button == MouseButton::PRIMARY && state.image_dragging => {
                state.image_dragging = false;
            }
            Event::Scroll(se)
                if state.in_slideshow && se.is_mouse && se.scroll.y != 0.0 => {
                let old_zoom = state.image_zoom;
                let new_zoom = if se.scroll.y > 0.0 {
                    old_zoom / 1.1
                } else {
                    old_zoom * 1.1
                };
                let new_zoom = new_zoom.clamp(0.1, 8.0);
                if (new_zoom - old_zoom).abs() > 0.00001 {
                    if viewport_contains(&self.last_rect, se.abs) {
                        let k = new_zoom / old_zoom;
                        let rs = self.last_rect.size;
                        let cursor = se.abs - self.last_rect.pos;
                        let scaled = dvec2(rs.x * old_zoom, rs.y * old_zoom);
                        let top_left = state.image_pan + (rs - scaled) * 0.5;
                        let image_pos = cursor - top_left;
                        let scaled_new = dvec2(rs.x * new_zoom, rs.y * new_zoom);
                        let top_left_new = cursor - image_pos * k;
                        state.image_pan = top_left_new - (rs - scaled_new) * 0.5;
                    }
                    if (new_zoom - 1.0).abs() < 0.001 {
                        state.image_pan = dvec2(0.0, 0.0);
                    }
                    state.image_zoom = new_zoom;
                    store_zoom_state(state.image_zoom, state.image_pan);
                    self.redraw(cx);
                }
            }
            _ => {}
        }
        self.view.handle_event(cx, event, scope)
    }
}

// 光标锚定缩放时判断滚轮是否发生在视图区域内
fn viewport_contains(rect: &Rect, abs: DVec2) -> bool {
    rect.contains(abs)
}
