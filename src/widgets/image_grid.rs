use makepad_widgets::*;
use crate::actions::ThumbDecoded;
use crate::state::State;
use crate::thumb::{downscale_image, THUMB_MAX_DIM, THUMB_MAX_IN_FLIGHT, THUMB_MAX_TEXTURES};
use crate::widgets::image_card::ImageCardWidget;

// ---------------------------------------------------------------------------
// ImageGrid：外层 PortalList（flow: Down），每行一个 ImageRow
//   负责处理 ThumbDecoded（缩略图纹理更新与 LRU）—— 这是它触发的资源
// ---------------------------------------------------------------------------
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.ImageRowBase = #(ImageRow::register_widget(vm))
    mod.widgets.ImageRow = set_type_default() do mod.widgets.ImageRowBase{
        width: Fill,
        height: 330,
        PortalList {
            height: 330,
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
}

#[derive(Script, ScriptHook, Widget)]
pub struct ImageGrid {
    #[deref]
    view: View,
}

impl Widget for ImageGrid {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        crate::dbg("GRID draw_walk called");
        let rect = cx.peek_walk_turtle(walk);
        crate::dbg(&format!(
            "GRID rect pos=({:.0},{:.0}) size={:.0}x{:.0}",
            rect.pos.x, rect.pos.y, rect.size.x, rect.size.y
        ));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let state = scope.data.get_mut::<State>().unwrap();
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let grid_width = cx.peek_walk_turtle(walk).size.x.max(1.0);
                let spacing = 12.0;
                let card_width = 260.0;
                let cols = ((grid_width + spacing) / (card_width + spacing))
                    .floor()
                    .max(1.0) as usize;
                state.max_images_per_row = cols;
                list.set_item_range(cx, 0, state.num_rows());
                crate::dbg(&format!(
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
        self.view.handle_event(cx, event, scope);
        // 必须显式调用 widget_match_event，否则 handle_actions（ThumbDecoded 纹理更新）不会触发
        self.widget_match_event(cx, event, scope);
    }
}

impl WidgetMatchEvent for ImageGrid {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, scope: &mut Scope) {
        // 缩略图解码完成 → 更新纹理并维护 LRU（由 ImageGrid 负责其触发的资源）
        for action in actions.iter() {
            if let Some(t) = action.downcast_ref::<ThumbDecoded>() {
                let state = scope.data.get_mut::<State>().unwrap();
                // 丢弃过期代际的解码结果（来自之前文件夹的后台线程）。
                if t.generation != state.generation {
                    continue;
                }
                let mut ib = ImageBuffer::default();
                ib.width = t.width;
                ib.height = t.height;
                ib.data = t.data.clone();
                let tex = ib.into_new_texture(cx);
                state.thumb_textures.insert(t.image_idx, tex);
                state.thumb_order.push(t.image_idx);
                state.thumb_in_flight.remove(&t.image_idx);
                while state.thumb_textures.len() > THUMB_MAX_TEXTURES {
                    if let Some(victim) = state.thumb_order.first().copied() {
                        state.thumb_order.remove(0);
                        state.thumb_textures.remove(&victim);
                    }
                }
                self.redraw(cx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ImageRow：内层 PortalList（flow: Right），每项是 ImageCardWidget
// ---------------------------------------------------------------------------
#[derive(Script, ScriptHook, Widget)]
pub struct ImageRow {
    #[deref]
    view: View,
}

impl Widget for ImageRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let (num_items, first_idx) = {
            let state = scope.data.get_mut::<State>().unwrap();
            let row_idx = *scope.props.get::<usize>().unwrap();
            (
                state.num_images_for_row(row_idx),
                state.first_image_idx_for_row(row_idx),
            )
        };

        let rect = cx.peek_walk_turtle(walk);
        let card_width = 260.0_f64;
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

                    let (filtered_image_idx, trigger_spawn, attach_tex, spawn_path, file_name, generation) = {
                        let state = scope.data.get_mut::<State>().unwrap();
                        let generation = state.generation;
                        let filtered_image_idx = state.filtered_image_idxs[image_idx];
                        let trigger_spawn = if state.loaded_image_idxs.insert(filtered_image_idx) {
                            if state.thumb_in_flight.len() < THUMB_MAX_IN_FLIGHT {
                                state.thumb_in_flight.insert(filtered_image_idx);
                                true
                            } else {
                                state.loaded_image_idxs.remove(&filtered_image_idx);
                                false
                            }
                        } else {
                            false
                        };
                        let attach_tex = state.thumb_textures.get(&filtered_image_idx).cloned();
                        let spawn_path = if trigger_spawn {
                            Some(state.image_paths[filtered_image_idx].clone())
                        } else {
                            None
                        };
                        let file_name = state.image_paths[filtered_image_idx]
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default()
                            .to_string();
                        (filtered_image_idx, trigger_spawn, attach_tex, spawn_path, file_name, generation)
                    };

                    let card = list.item(cx, item_idx, live_id!(ImageCard));
                    let mut do_set_tex = false;
                    if let Some(mut c) = card.borrow_mut::<ImageCardWidget>() {
                        // image_index 统一存“原始图片索引”（image_paths 下标），
                        // 与文件树点击、App::handle_open_image 的语义一致。
                        c.image_index = filtered_image_idx;
                        if attach_tex.is_some()
                            && c.last_thumb_idx != Some(filtered_image_idx) {
                            c.last_thumb_idx = Some(filtered_image_idx);
                            do_set_tex = true;
                        }
                    }
                    if do_set_tex
                        && let Some(tex) = attach_tex {
                            card.image(cx, ids!(thumb)).set_texture(cx.cx, Some(tex));
                        }
                    card.label(cx, ids!(img_name)).set_text(cx, &file_name);
                    if trigger_spawn
                        && let Some(image_path) = spawn_path {
                            std::thread::spawn(move || {
                                if let Ok(bytes) = std::fs::read(&image_path)
                                    && let Ok(buf) = decode_image_from_data(&bytes)
                                        && let Some((w, h, data)) =
                                            downscale_image(&buf, THUMB_MAX_DIM) {
                                        Cx::post_action(ThumbDecoded {
                                            image_idx: filtered_image_idx,
                                            width: w,
                                            height: h,
                                            data,
                                            generation,
                                        });
                                    }
                            });
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
