//! Rune 版 UI：左侧文件树 + 右侧工具栏/扫描进度条/动态网格，以及全屏幻灯片。
//! 布局与配色对齐 Makepad 版 `src/app.rs` 的 DSL。

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;

use rune::{
    Color, Cx, Drag, El, FileDialogOptions, GrowthSpec, Phase, Pixels, ScrollSpec, SurfaceFit,
    TextStyle, Tx, WheelEvent, col, el, row, text,
};

use crate::{State, actions, scan};

const CARD_W: f32 = 260.0;
const CARD_H: f32 = 300.0;
const GAP: f32 = 12.0;
const SIDE_PAD: f32 = 17.0;
const ROW_H: f32 = 312.0;
const TREE_ROW_H: f32 = 24.0;
const SIDEBAR_W: f32 = 200.0;
/// 同时持有 RGBA 数据的缩略图上限（可见行 + 最近滚出的一批）。
/// 256×171×4 ≈ 175 KB/张，128 张约 22 MB，与 Makepad 的 96 张纹理同量级。
pub const THUMB_LIVE_MAX: usize = 128;

fn c(hex: &str) -> Color {
    rune::color::hex(hex).expect("literal colour")
}

/// 动态列数 + 卡片等分宽度：左右留白一致，且不会在右侧多空一列。
fn grid_layout(content_w: f32) -> (usize, f32) {
    let inner_w = (content_w - 2.0 * SIDE_PAD).max(CARD_W);
    let cols = (((inner_w + GAP) / (CARD_W + GAP)).floor().max(1.0)) as usize;
    let card_w = (inner_w - (cols - 1) as f32 * GAP) / cols as f32;
    (cols, card_w)
}

fn button(key: &str, label: &str, f: impl Fn(&mut State) + 'static) -> El {
    el()
        .key(key)
        .h(26.0)
        .px(10.0)
        .radius(4.0)
        .fill(c("#474747"))
        .center()
        .on_press(f)
        .child(
            text(label)
                .font(TextStyle::new(12.0, 500.0))
                .color(c("#cbcbcb")),
        )
}

fn button_tx(key: &str, label: &str, f: impl Fn(&mut State, &mut Tx) + 'static) -> El {
    el()
        .key(key)
        .h(26.0)
        .px(10.0)
        .radius(4.0)
        .fill(c("#474747"))
        .center()
        .on_press_tx(f)
        .child(
            text(label)
                .font(TextStyle::new(12.0, 500.0))
                .color(c("#cbcbcb")),
        )
}

fn halo(x: f32, y: f32, d: f32, color: Color, alpha: f32) -> El {
    el()
        .absolute(x, y)
        .size(d, d)
        .radius(d * 0.5)
        .fill(color.alpha(alpha))
}

/// 用多层低透明度同心圆逼近 Makepad DSL 里的 RadGradient：层足够密，看不出环带。
fn background() -> Vec<El> {
    let spots = [
        (-180.0, -200.0, 760.0, c("#54d5ff")),
        (860.0, -120.0, 640.0, c("#ff8f3a")),
        (280.0, 260.0, 840.0, c("#d79cff")),
    ];
    const LAYERS: usize = 16;
    let mut out = Vec::with_capacity(spots.len() * LAYERS);
    for (x, y, d, color) in spots {
        for i in 0..LAYERS {
            let f = i as f32 / LAYERS as f32;
            let dd = d * (1.0 - 0.72 * f);
            let off = (d - dd) * 0.5;
            out.push(halo(x + off, y + off, dd, color, 0.008));
        }
    }
    out
}

pub fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let root = col().w_full().h_full().fill(c("#12131a")).children(background());
    if s.slideshow {
        root.child(slideshow_view(s))
    } else {
        root.child(browser_view(s))
    }
}

// ---------------------------------------------------------------------------
// 浏览器主界面
// ---------------------------------------------------------------------------

fn browser_view(s: &State) -> El {
    let mut body = row().w_full().grow(1.0).min_h(0.0);
    if s.sidebar_open && s.tree.is_some() {
        body = body.child(sidebar(s));
    }
    body.child(content(s))
}

fn sidebar(s: &State) -> El {
    let header = row()
        .w_full()
        .h(30.0)
        .pl(10.0)
        .center_y()
        .child(
            text("文件列表")
                .font(TextStyle::new(11.0, 500.0))
                .color(c("#cccccc")),
        );

    let rows = s.tree_rows.len();
    let list = col()
        .key("tree-rows")
        .w_full()
        .growth(GrowthSpec::default())
        .virtual_rows(
            rows,
            TREE_ROW_H,
            |_s: &State, i: usize| -> Cow<'_, str> { format!("t{i}").into() },
            |s: &State, i: usize| tree_row(s, i),
        );
    let viewport = col()
        .key("tree-viewport")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(list);

    col()
        .key("sidebar")
        .w(SIDEBAR_W)
        .h_full()
        .fill(c("#1a1b23"))
        .child(header)
        .child(viewport)
}

fn tree_row(s: &State, i: usize) -> El {
    let Some(r) = s.tree_rows.get(i) else {
        return el().h(TREE_ROW_H);
    };
    let indent = 6.0 + r.depth as f32 * 14.0;
    let marker = if r.is_dir {
        if r.open { "▾" } else { "▸" }
    } else {
        "·"
    };
    let selected = r.image_idx.is_some() && r.image_idx == actions::current_image_idx(s);
    let fill = if selected { c("#343644") } else { Color::TRANSPARENT };

    let mut line = row()
        .w_full()
        .h(TREE_ROW_H)
        .pl(indent)
        .gap(6.0)
        .center_y()
        .fill(fill)
        .child(
            text(marker)
                .font(TextStyle::new(10.0, 400.0))
                .color(c("#8a8fa5")),
        )
        .child(
            text(r.name.clone())
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#c8ccdd"))
                .truncate(),
        );

    if r.is_dir {
        let path = r.path.clone();
        line = line.on_press(move |s: &mut State| {
            if s.expanded.contains(&path) {
                s.expanded.remove(&path);
            } else {
                s.expanded.insert(path.clone());
            }
            crate::tree::rebuild(s);
        });
    } else if let Some(idx) = r.image_idx {
        line = line.on_press(move |s: &mut State| actions::open_image(s, idx));
    }
    line
}

fn content(s: &State) -> El {
    let mut out = col()
        .key("content")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .child(toolbar(s));
    if s.scan_active {
        out = out.child(scan_bar(s));
    }
    out.child(grid(s))
}

fn toolbar(s: &State) -> El {
    let count = if s.scan_active {
        format!("已发现: {}张", s.image_paths.len())
    } else {
        format!("共: {}张", s.image_paths.len())
    };
    row()
        .key("toolbar")
        .w_full()
        .h(36.0)
        .px(5.0)
        .gap(8.0)
        .center_y()
        .fill(c("#1f2029"))
        .child(button("sidebar-toggle", "☰ 文件列表", actions::toggle_sidebar))
        .child(button_tx("open-folder", "打开文件夹", |_: &mut State, tx: &mut Tx| {
            let options = FileDialogOptions {
                title: "选择图片文件夹".to_string(),
                ..Default::default()
            };
            tx.open_folder(options, |s: &mut State, result, _| {
                if let Ok(Some(path)) = result {
                    scan::start(s, path);
                }
            });
        }))
        .child(
            text(count)
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#c8ccdd")),
        )
        .child(el().grow(1.0))
        .child(button(
            "slideshow-button",
            "幻灯片模式",
            actions::open_slideshow,
        ))
}

fn scan_bar(s: &State) -> El {
    let ratio = if s.scan_total == 0 {
        0.0
    } else {
        (s.scan_visited as f32 / s.scan_total as f32).min(1.0)
    };
    let pct = (ratio * 100.0).round() as usize;
    let spinner = ["◐", "◓", "◑", "◒"][((s.clock * 10.0) as usize) % 4];
    let status = if s.scan_total == 0 {
        "正在统计文件数…".to_string()
    } else {
        let short: String = s.scan_current.chars().take(32).collect();
        format!("扫描中 {}/{} · {}", s.scan_visited, s.scan_total, short)
    };
    let track = el()
        .w(180.0)
        .h(8.0)
        .radius(4.0)
        .fill(c("#33343f"))
        .child(
            el()
                .w((180.0 * ratio).max(0.0))
                .h(8.0)
                .radius(4.0)
                .fill(c("#54d5ff")),
        );

    row()
        .key("scan-bar")
        .w_full()
        .h(32.0)
        .px(12.0)
        .gap(8.0)
        .center_y()
        .fill(c("#1a1a1f"))
        .child(
            text(spinner)
                .font(TextStyle::new(15.0, 400.0))
                .color(c("#54d5ff")),
        )
        .child(
            text(status)
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#cccccc"))
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(track)
        .child(
            text(format!("{pct}%"))
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#cccccc")),
        )
}

fn grid(s: &State) -> El {
    let (cols, card_w) = grid_layout(s.grid_w);
    let rows = s.filtered.len().div_ceil(cols);

    let list = col()
        .key("grid-rows")
        .w_full()
        .px(SIDE_PAD)
        .growth(GrowthSpec::default())
        .virtual_rows(
            rows,
            ROW_H,
            |_s: &State, i: usize| -> Cow<'_, str> { format!("g{i}").into() },
            move |s: &State, r: usize| grid_row(s, r, cols, card_w),
        )
        // 可见行（上下各留 2 行缓冲）才请求解码；持有 RGBA 的索引额外保留
        // 最近滚出的一批，总数由 THUMB_LIVE_MAX 限死。
        // 之前是「每张图都预备一个 Pixels 且永不释放」，滚动一个大文件夹时
        // CPU 侧会长期堆着成百上千份缩略图，静止后只靠图集回收页面部分回落。
        .on_visible_rows(move |s: &mut State, visible: Range<usize>| {
            const ROW_PAD: usize = 2;
            let start = visible.start.saturating_sub(ROW_PAD);
            let end = visible.end.saturating_add(ROW_PAD);
            let mut keep = HashSet::new();
            for r in start..end {
                for c in 0..cols {
                    let pos = r * cols + c;
                    let Some(&idx) = s.filtered.get(pos) else {
                        break;
                    };
                    keep.insert(idx);
                }
            }
            // 记下「应当显示」的索引集，供 `poll` 每帧补发被在途上限吞掉的请求。
            s.thumb_keep = keep.clone();

            // 新进入缓冲区：确保句柄存在，取回已在缓存里的结果，再请求解码。
            // 只有状态真正变化时才 set / request，避免 layout 每次回调都重复解码。
            let entering: Vec<usize> = keep.difference(&s.thumb_live).copied().collect();
            for idx in entering {
                let px = s
                    .thumb_pixels
                    .entry(idx)
                    .or_insert_with(Pixels::new)
                    .clone();
                s.thumb_live.insert(idx);
                s.thumb_recent.retain(|i| *i != idx);
                s.thumb_recent.push_back(idx);
                if let Some((w, h, rgba)) = s.thumb_cache.take_rgba(idx) {
                    // 之前滚出去时退回缓存的数据，直接零拷贝取用。
                    px.set(w as u32, h as u32, rgba);
                } else if let Some(path) = s.image_paths.get(idx).cloned() {
                    s.thumb_cache.request(idx, path);
                }
            }

            // 只对超上限、且已不在可见缓冲区的旧索引清空像素。刚滚出的卡片
            // 暂时保留内容，滚回来立刻显示（不闪空白、不重新解码）。
            let mut budget = s.thumb_recent.len() + keep.len() + 1;
            while s.thumb_live.len() > THUMB_LIVE_MAX && budget > 0 {
                budget -= 1;
                let Some(old) = s.thumb_recent.pop_front() else {
                    break;
                };
                if keep.contains(&old) {
                    s.thumb_recent.push_back(old);
                    continue;
                }
                if let Some(px) = s.thumb_pixels.get(&old) {
                    px.set(0, 0, Vec::new());
                }
                s.thumb_live.remove(&old);
            }
            false
        });

    col()
        .key("grid-viewport")
        .w_full()
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        // 布局完成后把真实宽度写回 state：侧栏展开/收起或窗口缩放时，列数会跟着重算。
        .on_layout(|s: &mut State, rect: [f32; 4]| {
            if (rect[2] - s.grid_w).abs() > 0.5 {
                s.grid_w = rect[2];
            }
        })
        .child(list)
}

fn grid_row(s: &State, r: usize, cols: usize, card_w: f32) -> El {
    let first = r * cols;
    let n = cols.min(s.filtered.len().saturating_sub(first));
    let mut line = row().w_full().h(ROW_H).gap(GAP).center();
    for c in 0..n {
        let pos = first + c;
        let idx = s.filtered[pos];
        line = line.child(card(s, idx, card_w));
    }
    line
}

fn card(s: &State, idx: usize, card_w: f32) -> El {
    let name = s
        .image_paths
        .get(idx)
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let pixels = s.thumb_pixels.get(&idx).cloned().unwrap_or_default();

    let thumb = el()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(c("#1b1c24"))
        .pixels_fit(&pixels, SurfaceFit::Fill);
    let label = row()
        .w_full()
        .h(18.0)
        .center()
        .child(
            text(name)
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#b8bcd0"))
                .truncate(),
        );

    col()
        .key(format!("card-{idx}"))
        .size(card_w, CARD_H)
        .padding(10.0)
        .gap(6.0)
        .radius(12.0)
        .clip_rounded(12.0)
        .fill(c("#2a2b36"))
        .border(1.5, c("#3a3b4a"))
        .on_press(move |s: &mut State| actions::open_image(s, idx))
        .child(thumb)
        .child(label)
}

// ---------------------------------------------------------------------------
// 幻灯片（对应 Makepad 版 Slideshow + ZoomableView）
// ---------------------------------------------------------------------------

fn slideshow_view(s: &State) -> El {
    let mut stage = col()
        .key("stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(c("#0c0c10"))
        .min_w(0.0)
        .clip_rounded(0.0)
        .on_wheel(|s: &mut State, e: WheelEvent, _: &mut Tx| {
            let n = (e.dy.abs() / 200.0).clamp(0.005, 0.06);
            let factor = if e.dy > 0.0 {
                1.0 / (1.0 + n)
            } else {
                1.0 + n
            };
            actions::zoom(s, factor);
            true
        })
        .on_drag(|s: &mut State, d: &mut Drag, _: &mut Tx| match d.phase {
            Phase::Down => s.drag_last = (d.dx, d.dy),
            Phase::Move => {
                s.pan_x += d.dx - s.drag_last.0;
                s.pan_y += d.dy - s.drag_last.1;
                s.drag_last = (d.dx, d.dy);
            }
            Phase::Up => s.drag_last = (0.0, 0.0),
        })
        .on_double_press(|s: &mut State| {
            if (s.zoom - 1.0).abs() < 0.001 {
                s.zoom = 2.0;
            } else {
                actions::reset_zoom(s);
            }
        });

    if s.img_w > 0.0 {
        // pixels_fit(Fit) 自动按图片比例居中留黑边；pan 用 absolute 偏移，zoom 用 scale
        // 绕中心缩放，都不需要预先知道舞台尺寸。
        stage = stage.child(
            el()
                .absolute(s.pan_x, s.pan_y)
                .w_full()
                .h_full()
                .pixels_fit(&s.full_pixels, SurfaceFit::Fit)
                .scale(s.zoom),
        );
    } else {
        stage = stage.child(
            col()
                .w_full()
                .h_full()
                .center()
                .child(
                    text(if s.full_loading { "加载中…" } else { "无图片" })
                        .font(TextStyle::new(16.0, 400.0))
                        .color(c("#8a8fa5")),
                ),
        );
    }

    let name = actions::current_image_idx(s)
        .and_then(|idx| s.image_paths.get(idx))
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "幻灯片模式".to_string());

    let bar = row()
        .key("slideshow-bar")
        .w_full()
        .h(36.0)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .fill(c("#171820"))
        .child(button("slideshow-close", "返回 (Esc)", actions::close_slideshow))
        .child(
            text(name)
                .font(TextStyle::new(12.0, 400.0))
                .color(c("#c8ccdd"))
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(
            text("← → 切换 · 滚轮缩放 · 拖拽平移 · +/-/0 · 双击缩放")
                .font(TextStyle::new(11.0, 400.0))
                .color(c("#8a8fa5")),
        );

    let left_button = el()
        .w(50.0)
        .h_full()
        .center()
        .fill(c("#ffffff0a"))
        .on_press(|s: &mut State| actions::navigate(s, -1))
        .child(
            text("‹")
                .font(TextStyle::new(30.0, 400.0))
                .color(c("#dddddd")),
        );
    let right_button = el()
        .w(50.0)
        .h_full()
        .center()
        .fill(c("#ffffff0a"))
        .on_press(|s: &mut State| actions::navigate(s, 1))
        .child(
            text("›")
                .font(TextStyle::new(30.0, 400.0))
                .color(c("#dddddd")),
        );

    col()
        .w_full()
        .h_full()
        .fill(c("#0a0a0e"))
        .child(bar)
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(left_button)
                .child(stage)
                .child(right_button),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_content_width_without_an_empty_column() {
        for width in [320.0, 640.0, 1080.0, 1280.0, 1920.0] {
            let (cols, card_w) = grid_layout(width);
            assert!(cols >= 1);
            let used = cols as f32 * card_w + (cols - 1) as f32 * GAP;
            let inner = (width - 2.0 * SIDE_PAD).max(CARD_W);
            assert!(
                (used - inner).abs() < 0.01,
                "width {width}: used {used} but had {inner}"
            );
            assert!(card_w > 0.0);
        }
    }

    #[test]
    fn wider_content_never_fewer_columns() {
        let a = grid_layout(600.0).0;
        let b = grid_layout(1600.0).0;
        assert!(b >= a);
    }
}
