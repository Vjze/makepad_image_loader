use makepad_widgets::*;
use crate::actions::OpenImage;

// ---------------------------------------------------------------------------
// ImageCardWidget：圆角卡片，图片悬浮在卡片内（图片本身也圆角），下方居中文件名
//   - 卡片背景/圆角/边框由 RoundedView 的 draw_bg 绘制
//   - 图片圆角通过覆盖 Image 的 draw_bg shader（Sdf2d 圆角 mask）实现
//   - hover 放大 + 高亮由 animator 驱动
// ---------------------------------------------------------------------------
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ImageCardWidgetBase = #(ImageCardWidget::register_widget(vm))
    mod.widgets.ImageCardWidget = set_type_default() do mod.widgets.ImageCardWidgetBase{
        ..mod.widgets.RoundedView {
            width: 260,
            height: 300,
            flow: Down,
            spacing: 6.0,
            padding: 10.0,
            show_bg: true,

            // 卡片底色 + 圆角 + 边框
            draw_bg +: {
                color: #2a2b36
                border_radius: 12.0
                border_size: 1.5
                border_color: #3a3b4a
            }

            animator: Animator {
                // ignore_missing: true
                hover: {
                    default: @off
                    off: AnimatorState {
                        redraw: true
                        from: {all: Snap}
                        apply: {
                            width: 260,
                            height: 300,
                            draw_bg: {color: #2a2b36, border_color: #3a3b4a}
                        }
                    }
                    on: AnimatorState {
                        cursor: MouseCursor.Hand
                        redraw: true
                        from: {all: Forward {duration: 0.1}}
                        apply: {
                            width: 280,
                            height: 320,
                            draw_bg: {color: #343644, border_color: #5a7ac0}
                        }
                    }
                }
            }

            // 悬浮在卡片内、带圆角的缩略图
            thumb := Image {
                width: Fill,
                height: Fill,
                fit: ImageFit.CropToFill,
                margin: 4.0,
                draw_bg +: {
                    // Image 的 draw_bg（DrawImage）本身已有 f32 类型的 border_radius，
                    // 这里只需赋值；写成 uniform(8.0) 会变成一个 uniform 声明对象，
                    // 触发 "expected f32, got object" 的类型不匹配。
                    border_radius: 8.0
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, self.border_radius)
                        let color = self.get_color()
                        // shape < 0 在圆角内（signed distance 为负）；
                        // 反转 smoothstep 让圆角内保留、圆角外透明，边缘抗锯齿
                        let mask = 1.0 - smoothstep(0.0, 1.0, sdf.shape)
                        return Pal.premul(vec4(color.xyz, color.w * self.opacity * mask))
                    }
                }
            }

            // 图片下方居中的文件名
            img_name := Label {
                width: Fill,
                height: Fit,
                draw_text +: {
                    text_style +: {font_size: 12.0}
                    color: #b8bcd0
                }
                align: Align{x: 0.5 y: 0.5}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ImageCardWidget {
    #[deref]
    view: View,
    #[rust]
    pub image_index: usize,
    #[rust]
    pub is_hovering: bool,
    #[rust]
    pub last_thumb_idx: Option<usize>,
}

impl Widget for ImageCardWidget {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // 放大由 animator 改变 width/height 驱动，这里直接交给 view 绘制。
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // 用当前帧实时 area 做命中判断。缓存的 Area 会因 redraw_id 过期而失效
        // （is_valid 返回 false），导致点击 FingerUp 永远不触发。
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
