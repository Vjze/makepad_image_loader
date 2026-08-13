use makepad_widgets::*;
use crate::actions::OpenImage;

// ---------------------------------------------------------------------------
// ImageCardWidget：包裹固定尺寸卡片 View 的自定义 Widget
// ---------------------------------------------------------------------------
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ImageCardWidgetBase = #(ImageCardWidget::register_widget(vm))
    mod.widgets.ImageCardWidget = set_type_default() do mod.widgets.ImageCardWidgetBase{
        ..mod.widgets.RoundedView {
        width: 250,
        height: 250,
        flow: Down,
        align: Center,

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
                border_size: uniform(2.0)
                border_radius: uniform(12.5)
            }

            thumb := Image {
                width: Fill,
                height: Fill,
                fit: ImageFit.CropToFill,
                margin: Inset{ left: 8.0, top: 8.0, right: 8.0, bottom: 8.0 }
            }
            img_name := Label {
                width: Fill,
                height: Fit,
                draw_text.text_size: 14.0,
                margin: Inset{ left: 8.0, right: 8.0, bottom: 8.0 }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ImageCardWidget {
    #[deref]
    view: View,
    #[live]
    draw_bg: DrawColor,
    #[rust]
    pub image_index: usize,
    #[rust]
    pub is_hovering: bool,
    #[rust]
    pub last_thumb_idx: Option<usize>,
}

impl Widget for ImageCardWidget {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.peek_walk_turtle(walk);

        if self.is_hovering {
            let pad = 10.0;
            self.draw_bg.color = vec4(0.45, 0.60, 0.85, 0.20);
            self.draw_bg.draw_abs(
                cx,
                Rect {
                    pos: dvec2(rect.pos.x - pad, rect.pos.y - pad),
                    size: dvec2(rect.size.x + pad * 2.0, rect.size.y + pad * 2.0),
                },
            );

            cx.walk_turtle(walk);

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
            Hit::FingerUp(fe)
                if fe.is_over && fe.was_tap() && fe.is_primary_hit() => {
                    // 自身点击 → 发出高层 OpenImage 命令，由 App 协调
                    cx.widget_action(
                        self.widget_uid(),
                        OpenImage {
                            image_index: self.image_index,
                        },
                    );
                }
            _ => {}
        }
        self.view.handle_event(cx, event, scope);
    }
}

impl WidgetMatchEvent for ImageCardWidget {
    fn handle_actions(&mut self, _cx: &mut Cx, _actions: &Actions, _scope: &mut Scope) {}
}
