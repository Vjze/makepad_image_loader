//! 交互动作：文件夹选择、扫描启动、过滤、幻灯片切换与缩放。
//! 与 Makepad 版 `App` 的 `load_image_paths` / `filter_image_paths` / `set_current_image`
//! / `navigate_left|right` / `handle_open_image` 一一对应。

use crate::{FULL_MAX_DIM, State};

pub fn apply_filter(s: &mut State, query: &str) {
    s.filtered = s
        .image_paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.to_string_lossy().contains(query))
        .map(|(i, _)| i)
        .collect();
    s.direct_idx = None;
    s.current = if s.filtered.is_empty() { None } else { Some(0) };
    load_current(s);
}

pub fn toggle_sidebar(s: &mut State) {
    if s.tree.is_some() {
        s.sidebar_open = !s.sidebar_open;
    }
}

pub fn open_slideshow(s: &mut State) {
    if s.image_paths.is_empty() {
        return;
    }
    if s.current.is_none() && s.direct_idx.is_none() {
        s.current = Some(0);
        load_current(s);
    }
    s.slideshow = true;
}

pub fn close_slideshow(s: &mut State) {
    s.slideshow = false;
}

/// 缩略图卡片 / 文件树点击：`target_idx` 是原始图片索引（image_paths 下标）。
pub fn open_image(s: &mut State, target_idx: usize) {
    if target_idx >= s.image_paths.len() {
        return;
    }
    let pos = s.filtered.iter().position(|&i| i == target_idx);
    s.current = pos;
    s.direct_idx = if pos.is_none() { Some(target_idx) } else { None };
    s.slideshow = true;
    reset_zoom(s);
    load_current(s);
}

pub fn navigate(s: &mut State, delta: isize) {
    let Some(pos) = s.current else {
        return;
    };
    let next = pos as isize + delta;
    if next < 0 || next >= s.filtered.len() as isize {
        return;
    }
    set_current(s, Some(next as usize));
}

fn set_current(s: &mut State, pos: Option<usize>) {
    s.current = pos;
    s.direct_idx = None;
    reset_zoom(s);
    load_current(s);
}

pub fn current_image_idx(s: &State) -> Option<usize> {
    s.direct_idx
        .or_else(|| s.current.and_then(|p| s.filtered.get(p).copied()))
}

/// 后台解码当前图片并直接写入 `full_pixels`（Rune 的 Pixels::set 可从任意线程调用，
/// 且会自动唤醒显示它的窗口，无需重跑 view）。
pub fn load_current(s: &mut State) {
    let Some(idx) = current_image_idx(s) else {
        return;
    };
    let Some(path) = s.image_paths.get(idx).cloned() else {
        return;
    };
    let Some(wake) = s.waker.clone() else {
        return;
    };

    s.full_token = s.full_token.wrapping_add(1);
    let token = s.full_token;
    let generation = s.generation;
    s.full_loading = true;

    let pixels = s.full_pixels.clone();
    let results = s.full_results.clone();
    std::thread::spawn(move || {
        // 解码时就缩到 FULL_MAX_DIM，不再物化全尺寸 RGBA8 再下采样。
        if let Ok((w, h, rgba)) = image_loader_core::decode::decode_rgba8_scaled(
            &path,
            FULL_MAX_DIM as u32,
        ) {
            pixels.set(w, h, rgba);
        }
        results.lock().unwrap().push((token, generation));
        wake();
    });
}

pub fn zoom(s: &mut State, factor: f32) {
    s.zoom = (s.zoom * factor).clamp(0.05, 25.0);
}

pub fn reset_zoom(s: &mut State) {
    s.zoom = 1.0;
    s.pan_x = 0.0;
    s.pan_y = 0.0;
    s.drag_last = (0.0, 0.0);
}
