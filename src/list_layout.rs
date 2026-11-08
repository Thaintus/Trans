use std::cell::Cell;
use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Controls::{
    HDM_GETITEMCOUNT, LVM_GETCOLUMNWIDTH, LVM_GETCOUNTPERPAGE, LVM_GETHEADER, LVM_GETITEMCOUNT,
    LVM_SETCOLUMNWIDTH,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetParent, GetWindowLongPtrW, GetWindowRect, SendMessageW, GWL_STYLE,
    SM_CXVSCROLL, SWP_NOSIZE, WINDOWPOS, WS_VSCROLL,
};

thread_local! {
    static PREPARING_LIST: Cell<usize> = const { Cell::new(0) };
}

struct PrepareScope(usize);

impl PrepareScope {
    fn new(hwnd: HWND) -> Self {
        Self(PREPARING_LIST.with(|current| current.replace(hwnd as usize)))
    }
}

impl Drop for PrepareScope {
    fn drop(&mut self) {
        PREPARING_LIST.with(|current| current.set(self.0));
    }
}

pub fn is_preparing(hwnd: HWND) -> bool {
    PREPARING_LIST.with(|current| current.get() == hwnd as usize)
}

// 列尾留一个物理像素，避免原生滚动范围在临界宽度舍入后溢出
pub fn column_capacity(client_width: i32) -> i32 {
    client_width.saturating_sub(1).max(0)
}

pub unsafe fn report_column_capacity(hwnd: HWND, client_width: i32) -> i32 {
    // 列表太矮时原生控件仍预留纵滚宽度，即使 WS_VSCROLL 没有出现
    let missing_vertical = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VSCROLL == 0
        && SendMessageW(hwnd, LVM_GETITEMCOUNT, 0, 0)
            > SendMessageW(hwnd, LVM_GETCOUNTPERPAGE, 0, 0).max(0);
    let reserve = if missing_vertical {
        GetSystemMetricsForDpi(SM_CXVSCROLL, GetDpiForWindow(hwnd).max(96)).max(0)
    } else {
        0
    };
    column_capacity(client_width.saturating_sub(reserve))
}

fn resize_capacity(
    client_width: i32,
    outer_width: i32,
    requested_width: i32,
    vertical_present: bool,
    scrollbar_width: i32,
) -> i32 {
    // 高度或列宽变化可能使纵向滚动条出现，先为其预留空间再提交新尺寸
    let reserve = if vertical_present {
        0
    } else {
        scrollbar_width.max(0)
    };
    let width = i64::from(client_width) + i64::from(requested_width.max(0))
        - i64::from(outer_width)
        - i64::from(reserve);
    column_capacity(width.clamp(0, i64::from(i32::MAX)) as i32)
}

fn shrink_columns(current: &[i32], capacity: i32) -> Vec<i32> {
    let widths: Vec<_> = current.iter().map(|&width| width.max(0)).collect();
    let total: i64 = widths.iter().map(|&width| i64::from(width)).sum();
    let capacity = capacity.max(0);
    if total <= i64::from(capacity) {
        return widths;
    }
    let mut prefix = 0i64;
    let mut allocated = 0i32;
    widths
        .into_iter()
        .map(|width| {
            prefix += i64::from(width);
            let end = (i128::from(prefix) * i128::from(capacity) / i128::from(total)) as i32;
            let result = end - allocated;
            allocated = end;
            result
        })
        .collect()
}

pub unsafe fn prepare_resize(hwnd: HWND, position: &WINDOWPOS, show_scrollbars: bool) {
    if position.flags & SWP_NOSIZE != 0 || is_preparing(hwnd) {
        return;
    }
    let mut outer: RECT = zeroed();
    let mut client: RECT = zeroed();
    if GetWindowRect(hwnd, &mut outer) == 0 || GetClientRect(hwnd, &mut client) == 0 {
        return;
    }
    if position.cx == outer.right - outer.left && position.cy == outer.bottom - outer.top {
        return;
    }
    let header = SendMessageW(hwnd, LVM_GETHEADER, 0, 0) as HWND;
    if header.is_null() {
        return;
    }
    let count = SendMessageW(header, HDM_GETITEMCOUNT, 0, 0);
    if !(2..=3).contains(&count) {
        return;
    }
    let capacity = resize_capacity(
        client.right - client.left,
        outer.right - outer.left,
        position.cx,
        GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VSCROLL != 0,
        if show_scrollbars {
            GetSystemMetricsForDpi(
                SM_CXVSCROLL,
                GetDpiForWindow(hwnd)
                    .max(GetDpiForWindow(GetParent(hwnd)))
                    .max(96),
            )
        } else {
            0
        },
    );
    let current: Vec<_> = (0..count as usize)
        .map(|index| SendMessageW(hwnd, LVM_GETCOLUMNWIDTH, index, 0) as i32)
        .collect();
    let targets = shrink_columns(&current, capacity);
    let _scope = PrepareScope::new(hwnd);
    for (index, (old, target)) in current.into_iter().zip(targets).enumerate() {
        if target < old {
            SendMessageW(hwnd, LVM_SETCOLUMNWIDTH, index, target as isize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_happens_before_the_native_client_width_changes() {
        for border in [0, 2, 4] {
            for scrollbar_width in [17, 26, 34] {
                for old_vertical in [false, true] {
                    for toggle_vertical in [false, true] {
                        let mut outer = 800;
                        let mut client =
                            outer - border - if old_vertical { scrollbar_width } else { 0 };
                        let mut vertical_present = old_vertical;
                        let mut columns = vec![client - 150, 70, 79];
                        for requested in [600, 320, 700, 95, 52, 500] {
                            let capacity = resize_capacity(
                                client,
                                outer,
                                requested,
                                vertical_present,
                                scrollbar_width,
                            );
                            let prepared = shrink_columns(&columns, capacity);
                            let new_vertical = if toggle_vertical {
                                !vertical_present
                            } else {
                                vertical_present
                            };
                            let next_client = (requested
                                - border
                                - if new_vertical { scrollbar_width } else { 0 })
                            .max(0);
                            assert!(prepared.iter().sum::<i32>() <= column_capacity(next_client));
                            assert!(prepared.iter().zip(&columns).all(|(new, old)| new <= old));
                            let budget = column_capacity(next_client);
                            columns =
                                vec![budget / 2, budget / 4, budget - budget / 2 - budget / 4];
                            outer = requested;
                            client = next_client;
                            vertical_present = new_vertical;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mini_hidden_percentage_column_stays_hidden_during_preshrink() {
        assert_eq!(shrink_columns(&[200, 0], 49), [49, 0]);
        assert_eq!(shrink_columns(&[0, 200], 49), [0, 49]);
        assert_eq!(shrink_columns(&[0, 0], 49), [0, 0]);
        assert_eq!(shrink_columns(&[10, 20], 50), [10, 20]);
    }

    #[test]
    fn shrinking_columns_never_creates_a_transient_overflow() {
        for current in [
            vec![301, 100, 100],
            vec![150, 38],
            vec![1, 0],
            vec![i32::MAX; 3],
        ] {
            for capacity in [0, 1, 2, 49, 299, i32::MAX] {
                let target = shrink_columns(&current, capacity);
                let mut applied = current.clone();
                for (index, width) in target.iter().copied().enumerate() {
                    applied[index] = width;
                    assert!(applied.iter().zip(&current).all(|(new, old)| new <= old));
                }
                assert!(
                    applied.iter().map(|&width| i64::from(width)).sum::<i64>()
                        <= i64::from(capacity)
                );
            }
        }
    }
}
