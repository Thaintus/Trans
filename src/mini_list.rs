use std::cell::Cell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::localization::tr;
use crate::ui_theme;
use crate::window_control::WindowInfo;

#[derive(Clone)]
pub struct Entry {
    pub window: WindowInfo,
    pub token: usize,
    pub title: String,
    pub transparency: u8,
    pub image: i32,
}

// HWND 和图标列表由 App 管理，此处仅维护行数据和布局
pub struct MiniList {
    hwnd: HWND,
    entries: Vec<Entry>,
    dirty: bool,
    fitting: Cell<bool>,
}

struct RedrawGuard {
    hwnd: HWND,
    suspended: bool,
}

impl RedrawGuard {
    unsafe fn new(hwnd: HWND) -> Self {
        // 隐藏列表不能发送 WM_SETREDRAW(TRUE)，否则会重新变为可见
        let suspended = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0;
        if suspended {
            SendMessageW(hwnd, WM_SETREDRAW, 0, 0);
        }
        Self { hwnd, suspended }
    }
}

impl Drop for RedrawGuard {
    fn drop(&mut self) {
        if self.suspended {
            unsafe {
                SendMessageW(self.hwnd, WM_SETREDRAW, 1, 0);
            }
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn same_window(left: &WindowInfo, right: &WindowInfo) -> bool {
    left.hwnd == right.hwnd
        && left.process_id == right.process_id
        && left.thread_id == right.thread_id
}

fn same_entry(left: &Entry, right: &Entry) -> bool {
    same_window(&left.window, &right.window) && left.token == right.token
}

unsafe fn set_cell(hwnd: HWND, row: usize, column: i32, value: &str) -> bool {
    let mut text = wide(value);
    let item = LVITEMW {
        iSubItem: column,
        pszText: text.as_mut_ptr(),
        ..zeroed()
    };
    SendMessageW(
        hwnd,
        LVM_SETITEMTEXTW,
        row,
        &item as *const LVITEMW as isize,
    ) != 0
}

impl MiniList {
    pub unsafe fn new(hwnd: HWND) -> Result<Self, String> {
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return Err(tr(
                "迷你窗口列表尚未创建",
                "The mini window list has not been created",
            )
            .into());
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            ((style & !LVS_TYPEMASK)
                | LVS_REPORT
                | LVS_NOCOLUMNHEADER
                | LVS_SINGLESEL
                | LVS_SHOWSELALWAYS
                | LVS_SHAREIMAGELISTS) as isize,
        );
        SendMessageW(hwnd, LVM_SETBKCOLOR, 0, ui_theme::SURFACE as isize);
        SendMessageW(hwnd, LVM_SETTEXTBKCOLOR, 0, ui_theme::SURFACE as isize);
        SendMessageW(hwnd, LVM_SETTEXTCOLOR, 0, ui_theme::TEXT as isize);
        // 原生标签浮层会对极窄列反复测量自绘名称，禁用以避免提示布局循环
        let extended = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
        SendMessageW(
            hwnd,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (extended | LVS_EX_LABELTIP) as usize,
            extended as isize,
        );
        for (index, name) in [tr("窗口名称", "Window"), tr("透明度", "%")]
            .iter()
            .enumerate()
        {
            let mut text = wide(name);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
                fmt: if index == 0 {
                    LVCFMT_LEFT
                } else {
                    LVCFMT_RIGHT
                },
                cx: 1,
                pszText: text.as_mut_ptr(),
                ..zeroed()
            };
            if SendMessageW(
                hwnd,
                LVM_INSERTCOLUMNW,
                index,
                &column as *const LVCOLUMNW as isize,
            ) != index as isize
            {
                for _ in 0..index {
                    SendMessageW(hwnd, LVM_DELETECOLUMN, 0, 0);
                }
                return Err(tr(
                    "无法初始化迷你窗口列表",
                    "Cannot initialize the mini window list",
                )
                .into());
            }
        }
        if !crate::list_scrollbar::install_without_bars(hwnd) {
            SendMessageW(hwnd, LVM_DELETECOLUMN, 1, 0);
            SendMessageW(hwnd, LVM_DELETECOLUMN, 0, 0);
            return Err(tr(
                "无法初始化迷你列表滚动行为",
                "Cannot initialize mini list scrolling",
            )
            .into());
        }
        let list = Self {
            hwnd,
            entries: Vec::new(),
            dirty: true,
            fitting: Cell::new(false),
        };
        list.fit_columns();
        Ok(list)
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        if self.dirty {
            None
        } else {
            self.entries.get(index)
        }
    }

    pub unsafe fn sync(&mut self, entries: Vec<Entry>) -> Result<(), String> {
        let rebuild = self.dirty
            || entries.len() != self.entries.len()
            || !entries
                .iter()
                .zip(&self.entries)
                .all(|(left, right)| same_entry(left, right));
        let old_top = SendMessageW(self.hwnd, LVM_GETTOPINDEX, 0, 0).max(0) as usize;
        let top_entry = self.entry(old_top).cloned();
        let selected = SendMessageW(
            self.hwnd,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_SELECTED as isize,
        );
        let selected_entry = (selected >= 0)
            .then(|| self.entry(selected as usize).cloned())
            .flatten();
        // 修改期间禁止通过行号取旧数据，失败后只有完整同步成功才恢复访问
        self.dirty = true;
        let redraw = RedrawGuard::new(self.hwnd);
        let updated = self.update_rows(&entries, rebuild);
        if !updated {
            SendMessageW(self.hwnd, LVM_DELETEALLITEMS, 0, 0);
            self.entries.clear();
            drop(redraw);
            self.fit_columns();
            InvalidateRect(self.hwnd, null(), 1);
            return Err(tr(
                "无法更新迷你项目列表，请展开完整界面后重新进入迷你模式",
                "Cannot update the mini list; expand the full interface and re-enter mini mode",
            )
            .into());
        }
        self.entries = entries;
        self.dirty = false;
        if rebuild {
            let selected = selected_entry.as_ref().and_then(|previous| {
                self.entries
                    .iter()
                    .find(|entry| same_entry(entry, previous))
            });
            self.select(selected.map(|entry| &entry.window));
        }
        drop(redraw);
        self.fit_columns();
        if rebuild && !self.entries.is_empty() {
            let top = top_entry
                .as_ref()
                .and_then(|previous| {
                    self.entries
                        .iter()
                        .position(|entry| same_entry(entry, previous))
                })
                .unwrap_or_else(|| old_top.min(self.entries.len() - 1));
            let mut bounds = RECT {
                left: LVIR_BOUNDS as i32,
                ..zeroed()
            };
            if SendMessageW(
                self.hwnd,
                LVM_GETITEMRECT,
                0,
                &mut bounds as *mut RECT as isize,
            ) != 0
            {
                let row_height = bounds.bottom - bounds.top;
                let current_top = SendMessageW(self.hwnd, LVM_GETTOPINDEX, 0, 0).max(0);
                let distance = (top as i64 - current_top as i64) * i64::from(row_height);
                if row_height > 0 && distance != 0 {
                    SendMessageW(
                        self.hwnd,
                        LVM_SCROLL,
                        0,
                        distance.clamp(i32::MIN as i64, i32::MAX as i64) as isize,
                    );
                }
            }
        }
        // 徽标映射由 App 单独维护，内容相同时仍允许重绘状态符号
        RedrawWindow(self.hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
        Ok(())
    }

    unsafe fn update_rows(&self, entries: &[Entry], rebuild: bool) -> bool {
        if entries.len() > i32::MAX as usize || IsWindow(self.hwnd) == 0 {
            return false;
        }
        if rebuild && SendMessageW(self.hwnd, LVM_DELETEALLITEMS, 0, 0) == 0 {
            return false;
        }
        for (index, entry) in entries.iter().enumerate() {
            if rebuild {
                let mut text = wide(&entry.title);
                let item = LVITEMW {
                    mask: LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE | LVIF_INDENT,
                    iItem: index as i32,
                    pszText: text.as_mut_ptr(),
                    lParam: entry.window.hwnd as isize,
                    iImage: entry.image,
                    iIndent: 0,
                    ..zeroed()
                };
                if SendMessageW(
                    self.hwnd,
                    LVM_INSERTITEMW,
                    0,
                    &item as *const LVITEMW as isize,
                ) != index as isize
                {
                    return false;
                }
            } else {
                let previous = &self.entries[index];
                if previous.title != entry.title && !set_cell(self.hwnd, index, 0, &entry.title) {
                    return false;
                }
                if previous.image != entry.image {
                    let item = LVITEMW {
                        mask: LVIF_PARAM | LVIF_IMAGE,
                        iItem: index as i32,
                        lParam: entry.window.hwnd as isize,
                        iImage: entry.image,
                        ..zeroed()
                    };
                    if SendMessageW(self.hwnd, LVM_SETITEMW, 0, &item as *const LVITEMW as isize)
                        == 0
                    {
                        return false;
                    }
                }
            }
            if (rebuild || self.entries[index].transparency != entry.transparency)
                && !set_cell(self.hwnd, index, 1, &format!("{}%", entry.transparency))
            {
                return false;
            }
        }
        true
    }

    pub unsafe fn select(&self, window: Option<&WindowInfo>) {
        if self.dirty {
            return;
        }
        let clear = LVITEMW {
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..zeroed()
        };
        SendMessageW(
            self.hwnd,
            LVM_SETITEMSTATE,
            usize::MAX,
            &clear as *const LVITEMW as isize,
        );
        if let Some(index) = window.and_then(|window| {
            self.entries
                .iter()
                .position(|entry| same_window(&entry.window, window))
        }) {
            let selected = LVITEMW {
                stateMask: LVIS_SELECTED | LVIS_FOCUSED,
                state: LVIS_SELECTED | LVIS_FOCUSED,
                ..zeroed()
            };
            SendMessageW(
                self.hwnd,
                LVM_SETITEMSTATE,
                index,
                &selected as *const LVITEMW as isize,
            );
        }
    }

    pub unsafe fn fit_columns(&self) {
        if IsWindow(self.hwnd) == 0
            || crate::list_layout::is_preparing(self.hwnd)
            || self.fitting.replace(true)
        {
            return;
        }
        let dpi = i64::from(GetDpiForWindow(self.hwnd).max(96));
        let percent_width = ((38 * dpi + 48) / 96) as i32;
        // 先缩窄再扩宽，复算滚动条出现或消失后的客户区宽度
        for _ in 0..3 {
            let mut bounds: RECT = zeroed();
            if GetClientRect(self.hwnd, &mut bounds) == 0 || bounds.right <= bounds.left {
                break;
            }
            let width = bounds.right - bounds.left;
            let capacity = crate::list_layout::column_capacity(width);
            // 极窄列表优先完整利用名称列，拉宽后恢复百分比列
            let percent = if i64::from(width) * 96 < 100 * dpi {
                0
            } else {
                percent_width.min(capacity)
            };
            let widths = [capacity - percent, percent];
            for shrinking in [true, false] {
                for (index, target) in widths.into_iter().enumerate() {
                    let current = SendMessageW(self.hwnd, LVM_GETCOLUMNWIDTH, index, 0) as i32;
                    if current != target && (current > target) == shrinking {
                        SendMessageW(self.hwnd, LVM_SETCOLUMNWIDTH, index, target as isize);
                    }
                }
            }
            let offset = GetScrollPos(self.hwnd, SB_HORZ);
            if offset != 0 {
                SendMessageW(self.hwnd, LVM_SCROLL, (-offset) as usize, 0);
            }
            if GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32 & WS_HSCROLL != 0 {
                ShowScrollBar(self.hwnd, SB_HORZ, 0);
            }
            let mut current: RECT = zeroed();
            if GetClientRect(self.hwnd, &mut current) == 0 || current.right - current.left == width
            {
                break;
            }
        }
        self.fitting.set(false);
    }
}
