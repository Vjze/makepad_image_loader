/// 组件向 App 发出的高层协调命令：
/// 组件在各自的 handle_event / WidgetMatchEvent 中处理完自身逻辑后，
/// 通过 `cx.widget_action` 发出，由 App 在 MatchEvent::handle_actions 中统一消费，
/// 完成需要跨组件协调的 UI 操作（切换页面、加载图片等）。
#[derive(Clone, Debug)]
pub struct OpenImage {
    pub image_index: usize,
}

/// 后台线程解码缩略图完成后回传的结果。
#[derive(Debug, Clone)]
pub struct ThumbDecoded {
    pub image_idx: usize,
    pub width: usize,
    pub height: usize,
    pub data: Vec<u32>,
}
