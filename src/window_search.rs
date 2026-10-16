use std::collections::HashSet;

use crate::localization::tr;
use crate::window_control::{ChangedWindowInfo, WindowInfo};

// 原标题和 PID 参与搜索，每个空白分词都必须匹配其中一项
pub fn filter_windows(
    windows: &[WindowInfo],
    tracked: &[ChangedWindowInfo],
    query: &str,
    self_process_id: u32,
) -> Vec<WindowInfo> {
    let tracked: HashSet<_> = tracked.iter().map(|row| identity(&row.window)).collect();
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    windows
        .iter()
        .filter(|window| window.process_id != self_process_id)
        .filter(|window| !tracked.contains(&identity(window)))
        .filter(|window| {
            let title = window.title.to_lowercase();
            let process_id = window.process_id.to_string();
            terms
                .iter()
                .all(|term| fuzzy_match(&title, term) || fuzzy_match(&process_id, term))
        })
        .cloned()
        .collect()
}

pub fn display_title(window: &WindowInfo) -> String {
    if window.title.trim().is_empty() {
        tr("无标题窗口", "Untitled window").into()
    } else {
        window.title.replace(['\r', '\n', '\t'], " ")
    }
}

fn identity(window: &WindowInfo) -> (usize, u32, u32) {
    (window.hwnd as usize, window.process_id, window.thread_id)
}

fn fuzzy_match(text: &str, query: &str) -> bool {
    if text.contains(query) {
        return true;
    }
    let mut expected = query.chars();
    let Some(mut wanted) = expected.next() else {
        return true;
    };
    // 按 Unicode 字符匹配，中文字符不会被 UTF-8 字节切分
    for actual in text.chars() {
        if actual == wanted {
            match expected.next() {
                Some(next) => wanted = next,
                None => return true,
            }
        }
    }
    false
}
