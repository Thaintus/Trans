use std::cell::Cell;
use std::mem::{size_of, zeroed};
use std::rc::Rc;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE,
    TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::ui_theme;

const SUBCLASS_ID: usize = 0x5452_534C;

struct ScrollGeometry {
    gutter: RECT,
    track: RECT,
    thumb_top: i32,
    thumb_height: i32,
    travel: i32,
    min_position: i32,
    max_position: i32,
    position: i32,
    dpi: u32,
}

struct ScrollState {
    alive: Cell<bool>,
    redraw_enabled: Cell<bool>,
    painting: Cell<bool>,
    hovered: Cell<bool>,
    tracking_mouse: Cell<bool>,
    drag_anchor: Cell<Option<i32>>,
    consume_button_up: Cell<bool>,
    last_paint: Cell<Option<PaintSignature>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PaintSignature {
    gutter: [i32; 4],
    track: [i32; 2],
    thumb: [i32; 2],
    position: i32,
    dpi: u32,
    hovered: bool,
    dragging: bool,
}

fn pixels(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(96) as i64 + 48) / 96) as i32
}

unsafe fn geometry(hwnd: HWND) -> Option<ScrollGeometry> {
    if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VSCROLL == 0 {
        return None;
    }
    let mut bar = SCROLLBARINFO {
        cbSize: size_of::<SCROLLBARINFO>() as u32,
        ..zeroed()
    };
    let mut info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        ..zeroed()
    };
    let mut outer: RECT = zeroed();
    if GetScrollBarInfo(hwnd, OBJID_VSCROLL, &mut bar) == 0
        || bar.rgstate[0] & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) != 0
        || GetScrollInfo(hwnd, SB_VERT, &mut info) == 0
        || GetWindowRect(hwnd, &mut outer) == 0
    {
        return None;
    }
    let gutter = RECT {
        left: bar.rcScrollBar.left - outer.left,
        top: bar.rcScrollBar.top - outer.top,
        right: bar.rcScrollBar.right - outer.left,
        bottom: bar.rcScrollBar.bottom - outer.top,
    };
    if gutter.right <= gutter.left || gutter.bottom <= gutter.top {
        return None;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut track = gutter;
    let header = SendMessageW(hwnd, LVM_GETHEADER, 0, 0) as HWND;
    let mut header_bounds: RECT = zeroed();
    if !header.is_null()
        && IsWindowVisible(header) != 0
        && GetWindowRect(header, &mut header_bounds) != 0
    {
        track.top = track.top.max(header_bounds.bottom - outer.top);
    }
    track.top = track.top.min(track.bottom);
    let margin = pixels(4, dpi).min((track.bottom - track.top) / 2);
    track.top += margin;
    track.bottom -= margin;
    let track_height = track.bottom - track.top;
    let range = (info.nMax as i64 - info.nMin as i64 + 1).max(1);
    let max_position = (info.nMax as i64 - info.nPage.saturating_sub(1) as i64)
        .max(info.nMin as i64)
        .min(i32::MAX as i64) as i32;
    let thumb_height = (track_height as i64 * info.nPage as i64 / range)
        .max(pixels(32, dpi) as i64)
        .min(track_height as i64) as i32;
    let travel = track_height - thumb_height;
    let max_offset = max_position as i64 - info.nMin as i64;
    let offset = (info.nPos as i64 - info.nMin as i64).clamp(0, max_offset);
    let thumb_top = track.top
        + if max_offset > 0 {
            (offset * travel as i64 / max_offset) as i32
        } else {
            0
        };
    Some(ScrollGeometry {
        gutter,
        track,
        thumb_top,
        thumb_height,
        travel,
        min_position: info.nMin,
        max_position,
        position: info.nPos,
        dpi,
    })
}

unsafe fn cursor_in_window(hwnd: HWND) -> Option<POINT> {
    let mut cursor: POINT = zeroed();
    let mut outer: RECT = zeroed();
    if GetCursorPos(&mut cursor) == 0 || GetWindowRect(hwnd, &mut outer) == 0 {
        return None;
    }
    cursor.x -= outer.left;
    cursor.y -= outer.top;
    Some(cursor)
}

fn inside(rect: &RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

unsafe fn draw_geometry(
    dc: HDC,
    shape: &ScrollGeometry,
    origin: POINT,
    hovered: bool,
    dragging: bool,
) {
    let gutter = RECT {
        left: shape.gutter.left - origin.x,
        top: shape.gutter.top - origin.y,
        right: shape.gutter.right - origin.x,
        bottom: shape.gutter.bottom - origin.y,
    };
    FillRect(dc, &gutter, GetStockObject(WHITE_BRUSH) as HBRUSH);
    if shape.track.bottom > shape.track.top && shape.max_position > shape.min_position {
        let track = RECT {
            left: shape.track.left - origin.x,
            top: shape.track.top - origin.y,
            right: shape.track.right - origin.x,
            bottom: shape.track.bottom - origin.y,
        };
        ui_theme::draw_scrollbar(
            dc,
            track,
            shape.thumb_top - origin.y,
            shape.thumb_height,
            shape.dpi,
            hovered,
            dragging,
        );
    }
}

impl ScrollState {
    unsafe fn paint(&self, hwnd: HWND) {
        self.paint_if_needed(hwnd, true);
    }

    unsafe fn paint_if_needed(&self, hwnd: HWND, force: bool) {
        if !self.alive.get()
            || !self.redraw_enabled.get()
            || IsWindowVisible(hwnd) == 0
            || self.painting.replace(true)
        {
            return;
        }
        if let Some(shape) = geometry(hwnd) {
            let signature = PaintSignature {
                gutter: [
                    shape.gutter.left,
                    shape.gutter.top,
                    shape.gutter.right,
                    shape.gutter.bottom,
                ],
                track: [shape.track.top, shape.track.bottom],
                thumb: [shape.thumb_top, shape.thumb_height],
                position: shape.position,
                dpi: shape.dpi,
                hovered: self.hovered.get() && IsWindowEnabled(hwnd) != 0,
                dragging: self.drag_anchor.get().is_some(),
            };
            if !force && self.last_paint.get() == Some(signature) {
                self.painting.set(false);
                return;
            }
            let dc = GetWindowDC(hwnd);
            if !dc.is_null() {
                let saved = SaveDC(dc);
                if saved != 0 {
                    IntersectClipRect(
                        dc,
                        shape.gutter.left,
                        shape.gutter.top,
                        shape.gutter.right,
                        shape.gutter.bottom,
                    );
                    let buffer_rect = RECT {
                        right: shape.gutter.right - shape.gutter.left,
                        bottom: shape.gutter.bottom - shape.gutter.top,
                        ..zeroed()
                    };
                    let presented =
                        ui_theme::PaintBuffer::new(dc, buffer_rect).is_some_and(|buffer| {
                            // 零原点缓冲避免 GDI+ 忽略 GDI 原点造成错位
                            draw_geometry(
                                buffer.dc(),
                                &shape,
                                POINT {
                                    x: shape.gutter.left,
                                    y: shape.gutter.top,
                                },
                                signature.hovered,
                                signature.dragging,
                            );
                            BitBlt(
                                dc,
                                shape.gutter.left,
                                shape.gutter.top,
                                buffer_rect.right,
                                buffer_rect.bottom,
                                buffer.dc(),
                                0,
                                0,
                                SRCCOPY,
                            ) != 0
                        });
                    if !presented {
                        draw_geometry(
                            dc,
                            &shape,
                            POINT { x: 0, y: 0 },
                            signature.hovered,
                            signature.dragging,
                        );
                    }
                    RestoreDC(dc, saved);
                    self.last_paint.set(Some(signature));
                }
                ReleaseDC(hwnd, dc);
            }
        } else {
            self.last_paint.set(None);
        }
        self.painting.set(false);
    }

    unsafe fn set_hovered(&self, hwnd: HWND, hovered: bool) {
        if self.hovered.replace(hovered) != hovered {
            self.paint(hwnd);
        }
    }

    unsafe fn cancel_drag(&self, hwnd: HWND) {
        if self.drag_anchor.take().is_some() && GetCapture() == hwnd {
            ReleaseCapture();
        }
    }

    unsafe fn begin_tracking(&self, hwnd: HWND) {
        if !self.tracking_mouse.get() {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            self.tracking_mouse.set(TrackMouseEvent(&mut tracking) != 0);
        }
    }

    unsafe fn drag_to(&self, hwnd: HWND, point: POINT, anchor: i32) {
        let Some(shape) = geometry(hwnd) else {
            self.cancel_drag(hwnd);
            return;
        };
        if shape.travel <= 0 || shape.max_position <= shape.min_position {
            self.cancel_drag(hwnd);
            return;
        }
        let thumb_offset =
            (point.y as i64 - anchor as i64 - shape.track.top as i64).clamp(0, shape.travel as i64);
        let max_offset = shape.max_position as i64 - shape.min_position as i64;
        // 当前普通报表列表的滚动单位为行，最小位置为零
        let target_top =
            (thumb_offset * max_offset + shape.travel as i64 / 2) / shape.travel as i64;
        let current_top = SendMessageW(hwnd, LVM_GETTOPINDEX, 0, 0).max(0);
        let mut row: RECT = RECT {
            left: LVIR_BOUNDS as i32,
            ..zeroed()
        };
        if SendMessageW(
            hwnd,
            LVM_GETITEMRECT,
            current_top as usize,
            &mut row as *mut RECT as LPARAM,
        ) == 0
        {
            return;
        }
        let row_height = row.bottom - row.top;
        if row_height <= 0 {
            return;
        }
        // 通过列表原生像素滚动保留行对齐，并避免 WM_VSCROLL 的 16 位位置截断
        let distance = ((target_top - current_top as i64) * row_height as i64)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        if distance != 0 {
            SendMessageW(hwnd, LVM_SCROLL, 0, distance as LPARAM);
        }
    }
}

pub unsafe fn install(hwnd: HWND) -> bool {
    let mut existing = 0usize;
    if GetWindowSubclass(hwnd, Some(scrollbar_proc), SUBCLASS_ID, &mut existing) != 0 {
        return true;
    }
    let state = Rc::new(ScrollState {
        alive: Cell::new(true),
        redraw_enabled: Cell::new(true),
        painting: Cell::new(false),
        hovered: Cell::new(false),
        tracking_mouse: Cell::new(false),
        drag_anchor: Cell::new(None),
        consume_button_up: Cell::new(false),
        last_paint: Cell::new(None),
    });
    let raw = Rc::into_raw(state);
    if SetWindowSubclass(hwnd, Some(scrollbar_proc), SUBCLASS_ID, raw as usize) == 0 {
        drop(Rc::from_raw(raw));
        return false;
    }
    true
}

unsafe extern "system" fn scrollbar_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    let raw = data as *const ScrollState;
    // 原生通知可能重入并销毁列表，每次回调持有独立引用直到退出
    Rc::increment_strong_count(raw);
    let state = Rc::from_raw(raw);
    match message {
        WM_NCDESTROY => {
            state.alive.set(false);
            state.drag_anchor.set(None);
            RemoveWindowSubclass(hwnd, Some(scrollbar_proc), subclass);
            drop(Rc::from_raw(raw));
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
        WM_SETREDRAW => {
            state.redraw_enabled.set(wparam != 0);
            if wparam == 0 {
                state.cancel_drag(hwnd);
            }
        }
        WM_NCPAINT => {
            // 列表没有原生边框，仅替换实际存在的非客户区纵向滚动条
            state.paint(hwnd);
            return 0;
        }
        WM_NCMOUSEMOVE if wparam == HTVSCROLL as usize => {
            if IsWindowEnabled(hwnd) != 0 {
                state.begin_tracking(hwnd);
                state.set_hovered(hwnd, true);
            }
            return 0;
        }
        WM_NCMOUSELEAVE => {
            state.tracking_mouse.set(false);
            state.set_hovered(hwnd, false);
            return 0;
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam == HTVSCROLL as usize => {
            state.consume_button_up.set(true);
            if IsWindowEnabled(hwnd) == 0 || !state.redraw_enabled.get() {
                return 0;
            }
            let (Some(shape), Some(point)) = (geometry(hwnd), cursor_in_window(hwnd)) else {
                return 0;
            };
            if shape.track.bottom <= shape.track.top
                || shape.max_position <= shape.min_position
                || !inside(&shape.track, point)
            {
                return 0;
            }
            state.set_hovered(hwnd, true);
            if point.y >= shape.thumb_top && point.y < shape.thumb_top + shape.thumb_height {
                state.drag_anchor.set(Some(point.y - shape.thumb_top));
                SetCapture(hwnd);
                if GetCapture() != hwnd {
                    state.drag_anchor.set(None);
                }
                state.paint(hwnd);
            } else {
                let command = if point.y < shape.thumb_top {
                    SB_PAGEUP
                } else {
                    SB_PAGEDOWN
                };
                SendMessageW(hwnd, WM_VSCROLL, command as WPARAM, 0);
            }
            return 0;
        }
        WM_MOUSEMOVE if state.drag_anchor.get().is_some() => {
            if IsWindowEnabled(hwnd) != 0 && GetCapture() == hwnd {
                if let (Some(point), Some(anchor)) =
                    (cursor_in_window(hwnd), state.drag_anchor.get())
                {
                    state.drag_to(hwnd, point, anchor);
                }
            } else {
                state.cancel_drag(hwnd);
                state.paint(hwnd);
            }
            return 0;
        }
        WM_LBUTTONUP | WM_NCLBUTTONUP if state.consume_button_up.replace(false) => {
            if let (Some(point), Some(anchor)) = (cursor_in_window(hwnd), state.drag_anchor.get()) {
                state.drag_to(hwnd, point, anchor);
            }
            state.cancel_drag(hwnd);
            if !state.alive.get() {
                return 0;
            }
            state.begin_tracking(hwnd);
            let hovered = geometry(hwnd)
                .zip(cursor_in_window(hwnd))
                .is_some_and(|(shape, point)| inside(&shape.gutter, point));
            state.hovered.set(hovered);
            state.paint(hwnd);
            return 0;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            // 上次在其他窗口松开时可能没有收到 UP，新手势恢复列表原生处理
            if state.consume_button_up.replace(false) {
                state.cancel_drag(hwnd);
            }
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            state.cancel_drag(hwnd);
            state.hovered.set(false);
            state.paint(hwnd);
        }
        WM_SIZE | WM_DPICHANGED_AFTERPARENT | WM_THEMECHANGED => {
            state.cancel_drag(hwnd);
            state.hovered.set(false);
        }
        WM_ENABLE | WM_SHOWWINDOW if wparam == 0 => {
            state.cancel_drag(hwnd);
            state.hovered.set(false);
            state.tracking_mouse.set(false);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_PAINT
            | WM_NCACTIVATE
            | WM_SIZE
            | WM_WINDOWPOSCHANGED
            | WM_DPICHANGED_AFTERPARENT
            | WM_THEMECHANGED
            | WM_STYLECHANGED
            | WM_SETFONT
            | WM_SETREDRAW
            | WM_SHOWWINDOW
            | WM_ENABLE
            | WM_VSCROLL
            | WM_MOUSEWHEEL
            | WM_KEYDOWN
            | WM_CHAR
            | WM_SETFOCUS
            | LVM_SCROLL
            | LVM_ENSUREVISIBLE
            | LVM_INSERTITEMA
            | LVM_INSERTITEMW
            | LVM_DELETEITEM
            | LVM_DELETEALLITEMS
            | LVM_SETITEMCOUNT
            | LVM_SETITEMSTATE
            | LVM_SETIMAGELIST
            | LVM_SETCOLUMNWIDTH
            | LVM_SETEXTENDEDLISTVIEWSTYLE
            | LVM_SETVIEW
    ) {
        state.paint(hwnd);
    } else if matches!(
        message,
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONUP | WM_MOUSEMOVE | WM_TIMER
    ) {
        // 原生选中半露行和边缘自动滚动可能在内部直接更新滚动位置
        state.paint_if_needed(hwnd, false);
    }
    result
}
