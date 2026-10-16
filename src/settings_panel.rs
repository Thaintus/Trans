use std::cell::Cell;
use std::marker::PhantomData;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::localization::tr;
use crate::ui_theme;

pub const SCROLL_CHANGED: u32 = WM_APP + 4;
const LAYOUT_NOTICE: u32 = WM_APP + 51;
const CHILD_SUBCLASS: usize = 0x5452_5350;
const CONTENT_HEIGHT: i32 = 464;
const SCROLL_LINE: i32 = 24;
const WHEEL_STEP: i32 = 120;
const SCROLL_GUTTER: i32 = 12;

struct ScrollGeometry {
    top: i32,
    bottom: i32,
    thumb_top: i32,
    thumb_height: i32,
    travel: i32,
}

fn pixels(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(96) as i64 + 48) / 96) as i32
}

struct PanelState {
    hwnd: Cell<HWND>,
    parent: HWND,
    dpi: Cell<u32>,
    bounds: Cell<Option<[i32; 4]>>,
    width: Cell<i32>,
    height: Cell<i32>,
    offset: Cell<i32>,
    applied_offset: Cell<i32>,
    wheel_remainder: Cell<i32>,
    scrollbar_hovered: Cell<bool>,
    tracking_mouse: Cell<bool>,
    drag_anchor: Cell<Option<i32>>,
    updating_metrics: Cell<bool>,
    notice_pending: Cell<bool>,
}

impl PanelState {
    fn max_offset(&self) -> i32 {
        (pixels(CONTENT_HEIGHT, self.dpi.get()) - self.height.get()).max(0)
    }

    fn content_width(&self) -> i32 {
        let gutter = if self.max_offset() > 0 {
            pixels(SCROLL_GUTTER, self.dpi.get())
        } else {
            0
        };
        (self.width.get() - gutter).max(0)
    }

    fn scroll_geometry(&self) -> Option<ScrollGeometry> {
        if self.max_offset() == 0 || self.width.get() <= 0 || self.height.get() <= 0 {
            return None;
        }
        let margin = pixels(4, self.dpi.get()).min(self.height.get() / 2);
        let top = margin;
        let bottom = self.height.get() - margin;
        let track_height = bottom - top;
        if track_height <= 0 {
            return None;
        }
        let content = pixels(CONTENT_HEIGHT, self.dpi.get());
        let thumb_height = ((track_height as i64 * self.height.get() as i64 / content as i64)
            as i32)
            .max(pixels(32, self.dpi.get()))
            .min(track_height);
        let travel = track_height - thumb_height;
        let thumb_top =
            top + (self.offset.get() as i64 * travel as i64 / self.max_offset() as i64) as i32;
        Some(ScrollGeometry {
            top,
            bottom,
            thumb_top,
            thumb_height,
            travel,
        })
    }

    fn scrollbar_hit(&self, x: i32, y: i32) -> bool {
        self.max_offset() > 0
            && x >= self.content_width()
            && x < self.width.get()
            && y >= 0
            && y < self.height.get()
    }

    unsafe fn invalidate_scrollbar(&self) {
        if self.hwnd.get().is_null() {
            return;
        }
        let rect = RECT {
            left: self.content_width(),
            top: 0,
            right: self.width.get(),
            bottom: self.height.get(),
        };
        InvalidateRect(self.hwnd.get(), &rect, 0);
    }

    unsafe fn set_hovered(&self, hovered: bool) {
        if self.scrollbar_hovered.replace(hovered) != hovered {
            self.invalidate_scrollbar();
        }
    }

    unsafe fn end_drag(&self) {
        if self.drag_anchor.take().is_some() {
            if GetCapture() == self.hwnd.get() {
                ReleaseCapture();
            }
            self.invalidate_scrollbar();
        }
    }

    unsafe fn mouse_move(&self, x: i32, y: i32) {
        if !self.tracking_mouse.get() {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd.get(),
                dwHoverTime: 0,
            };
            self.tracking_mouse.set(TrackMouseEvent(&mut tracking) != 0);
        }
        self.set_hovered(self.scrollbar_hit(x, y));
        if let Some(anchor) = self.drag_anchor.get() {
            if GetCapture() != self.hwnd.get() {
                self.end_drag();
                return;
            }
            if let Some(geometry) = self.scroll_geometry() {
                if geometry.travel > 0 {
                    let position = y
                        .saturating_sub(geometry.top)
                        .saturating_sub(anchor)
                        .clamp(0, geometry.travel);
                    let requested = ((position as i64 * self.max_offset() as i64
                        + geometry.travel as i64 / 2)
                        / geometry.travel as i64) as i32;
                    self.set_offset(requested);
                }
            }
        }
    }

    unsafe fn mouse_down(&self, x: i32, y: i32) -> bool {
        if !self.scrollbar_hit(x, y) {
            return false;
        }
        let Some(geometry) = self.scroll_geometry() else {
            return false;
        };
        self.set_hovered(true);
        if y >= geometry.thumb_top && y < geometry.thumb_top + geometry.thumb_height {
            if geometry.travel > 0 {
                self.drag_anchor.set(Some(y - geometry.thumb_top));
                SetCapture(self.hwnd.get());
                if GetCapture() != self.hwnd.get() {
                    self.drag_anchor.set(None);
                }
                self.invalidate_scrollbar();
            }
        } else {
            let line = pixels(SCROLL_LINE, self.dpi.get());
            let page = (self.height.get() - line).max(line);
            let requested = if y < geometry.thumb_top {
                self.offset.get().saturating_sub(page)
            } else {
                self.offset.get().saturating_add(page)
            };
            self.set_offset(requested);
        }
        true
    }

    fn set_dpi(&self, dpi: u32) -> bool {
        let dpi = dpi.max(96);
        let previous = self.dpi.replace(dpi);
        if previous == dpi {
            return false;
        }
        self.offset.set(
            ((self.offset.get() as i64 * dpi as i64 + previous as i64 / 2) / previous as i64)
                as i32,
        );
        true
    }

    unsafe fn request_layout(&self) {
        let hwnd = self.hwnd.get();
        if !hwnd.is_null() && !self.notice_pending.replace(true) {
            if PostMessageW(hwnd, LAYOUT_NOTICE, 0, 0) == 0 {
                self.notice_pending.set(false);
            }
        }
    }

    unsafe fn sync_metrics(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() || self.updating_metrics.replace(true) {
            return;
        }
        let mut rect: RECT = zeroed();
        if GetClientRect(hwnd, &mut rect) == 0 {
            self.updating_metrics.set(false);
            return;
        }
        let previous = (self.width.get(), self.height.get(), self.offset.get());
        let height = (rect.bottom - rect.top).max(0);
        self.width.set((rect.right - rect.left).max(0));
        self.height.set(height);
        self.offset
            .set(self.offset.get().clamp(0, self.max_offset()));
        if (previous.0, previous.1) != (self.width.get(), self.height.get()) {
            self.end_drag();
        }
        if self.max_offset() == 0 {
            self.end_drag();
            self.scrollbar_hovered.set(false);
        }
        self.updating_metrics.set(false);
        if previous != (self.width.get(), self.height.get(), self.offset.get()) {
            InvalidateRect(hwnd, null(), 0);
            self.request_layout();
        }
    }

    unsafe fn set_offset(&self, requested: i32) {
        let offset = requested.clamp(0, self.max_offset());
        if offset == self.offset.replace(offset) {
            return;
        }
        self.invalidate_scrollbar();
        // 控件统一由应用重新布局，避免直接滚动后与已缓存的控件坐标不一致
        self.request_layout();
    }

    unsafe fn vertical_scroll(&self, command: i32) {
        let offset = self.offset.get();
        let line = pixels(SCROLL_LINE, self.dpi.get());
        let page = (self.height.get() - line).max(line);
        let requested = match command {
            SB_LINEUP => offset.saturating_sub(line),
            SB_LINEDOWN => offset.saturating_add(line),
            SB_PAGEUP => offset.saturating_sub(page),
            SB_PAGEDOWN => offset.saturating_add(page),
            SB_TOP => 0,
            SB_BOTTOM => self.max_offset(),
            _ => return,
        };
        self.set_offset(requested);
    }

    unsafe fn mouse_wheel(&self, wparam: WPARAM) {
        let delta = (wparam >> 16) as u16 as i16 as i32;
        let accumulated = self.wheel_remainder.get().saturating_add(delta);
        let steps = accumulated / WHEEL_STEP;
        self.wheel_remainder.set(accumulated % WHEEL_STEP);
        if steps == 0 {
            return;
        }
        let mut lines = 3u32;
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            (&mut lines as *mut u32).cast(),
            0,
        );
        let distance = if lines == u32::MAX {
            self.height.get().max(1)
        } else {
            pixels(SCROLL_LINE, self.dpi.get()).saturating_mul(lines.min(i32::MAX as u32) as i32)
        };
        self.set_offset(
            self.offset
                .get()
                .saturating_sub(steps.saturating_mul(distance)),
        );
    }

    unsafe fn ensure_visible(&self, child: HWND) {
        let height = self.height.get();
        if height <= 0 || self.max_offset() == 0 {
            return;
        }
        let mut rect: RECT = zeroed();
        if GetWindowRect(child, &mut rect) == 0 {
            return;
        }
        MapWindowPoints(
            null_mut(),
            self.hwnd.get(),
            (&mut rect as *mut RECT).cast(),
            2,
        );
        let margin = pixels(4, self.dpi.get());
        // 子项尚未完成异步布局时，用已应用的偏移还原内容坐标
        let content_top = rect.top.saturating_add(self.applied_offset.get());
        let content_bottom = rect.bottom.saturating_add(self.applied_offset.get());
        let offset = self.offset.get();
        let requested = if content_top < offset.saturating_add(margin) {
            content_top.saturating_sub(margin)
        } else if content_bottom > offset.saturating_add(height - margin) {
            content_bottom.saturating_sub(height - margin)
        } else {
            return;
        };
        self.set_offset(requested);
    }

    unsafe fn draw(&self, dc: HDC, rect: &RECT) {
        FillRect(dc, rect, GetStockObject(WHITE_BRUSH));
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        SetDCPenColor(dc, ui_theme::BORDER);
        let dpi = self.dpi.get();
        let offset = self.applied_offset.get();
        let right = self.content_width() - 1;
        for (top, bottom) in [(36, 244), (284, 340), (388, 462)] {
            RoundRect(
                dc,
                1,
                pixels(top, dpi) - offset,
                right,
                pixels(bottom, dpi) - offset,
                pixels(12, dpi),
                pixels(12, dpi),
            );
        }
        RestoreDC(dc, saved);
        if let Some(geometry) = self.scroll_geometry() {
            ui_theme::draw_scrollbar(
                dc,
                RECT {
                    left: self.content_width(),
                    top: geometry.top,
                    right: self.width.get(),
                    bottom: geometry.bottom,
                },
                geometry.thumb_top,
                geometry.thumb_height,
                dpi,
                self.scrollbar_hovered.get(),
                self.drag_anchor.get().is_some(),
            );
        }
    }
}

pub struct SettingsPanel {
    state: Box<PanelState>,
    _thread: PhantomData<*mut ()>,
}

impl SettingsPanel {
    pub unsafe fn new(parent: HWND) -> Result<Self, String> {
        let class_name: Vec<u16> = "Trans.SettingsPanel.v1\0".encode_utf16().collect();
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(panel_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!(
                "{} {}",
                tr(
                    "无法初始化设置滚动区域，Windows 错误码",
                    "Cannot initialize the settings scroll area; Windows error"
                ),
                GetLastError()
            ));
        }
        let mut state = Box::new(PanelState {
            hwnd: Cell::new(null_mut()),
            parent,
            dpi: Cell::new(GetDpiForWindow(parent).max(96)),
            bounds: Cell::new(None),
            width: Cell::new(0),
            height: Cell::new(0),
            offset: Cell::new(0),
            applied_offset: Cell::new(0),
            wheel_remainder: Cell::new(0),
            scrollbar_hovered: Cell::new(false),
            tracking_mouse: Cell::new(false),
            drag_anchor: Cell::new(None),
            updating_metrics: Cell::new(false),
            notice_pending: Cell::new(false),
        });
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            null(),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            parent,
            null_mut(),
            instance,
            (&mut *state as *mut PanelState).cast(),
        );
        if hwnd.is_null() {
            return Err(format!(
                "{} {}",
                tr(
                    "无法创建设置滚动区域，Windows 错误码",
                    "Cannot create the settings scroll area; Windows error"
                ),
                GetLastError()
            ));
        }
        Ok(Self {
            state,
            _thread: PhantomData,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.state.hwnd.get()
    }

    pub unsafe fn set_bounds(&self, bounds: [i32; 4], dpi: u32) {
        if self.hwnd().is_null() {
            return;
        }
        let dpi_changed = self.state.set_dpi(dpi);
        if dpi_changed {
            self.state.end_drag();
        }
        if self.state.bounds.get() != Some(bounds) {
            let [x, y, width, height] = bounds;
            if SetWindowPos(
                self.hwnd(),
                null_mut(),
                x,
                y,
                width.max(0),
                height.max(0),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS,
            ) != 0
            {
                self.state.bounds.set(Some(bounds));
            }
        }
        self.state.sync_metrics();
        if dpi_changed {
            self.state.request_layout();
        }
    }

    pub unsafe fn client_width(&self) -> i32 {
        self.state.content_width()
    }

    pub fn scroll_offset(&self) -> i32 {
        self.state.offset.get()
    }

    pub fn commit_layout(&self, applied_offset: i32) {
        // 仅在该轮子项位置全部应用后记录，保留与实际 HWND 几何一致的偏移
        self.state.applied_offset.set(applied_offset);
    }

    pub unsafe fn show(&self, visible: bool) {
        let hwnd = self.hwnd();
        if hwnd.is_null() {
            return;
        }
        if visible {
            self.state.sync_metrics();
        } else {
            self.state.end_drag();
            self.state.set_hovered(false);
            self.state.tracking_mouse.set(false);
        }
        ShowWindow(hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
    }

    pub unsafe fn install_child(&self, child: HWND) -> bool {
        !child.is_null()
            && GetParent(child) == self.hwnd()
            && SetWindowSubclass(
                child,
                Some(child_proc),
                CHILD_SUBCLASS,
                (&*self.state as *const PanelState) as usize,
            ) != 0
    }
}

impl Drop for SettingsPanel {
    fn drop(&mut self) {
        let hwnd = self.hwnd();
        if !hwnd.is_null() {
            // Box 保持到窗口及所有子项销毁后，再释放子类过程引用的状态
            unsafe { DestroyWindow(hwnd) };
        }
    }
}

unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        let state = create.lpCreateParams as *const PanelState;
        if state.is_null() {
            return 0;
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        (*state).hwnd.set(hwnd);
    }
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PanelState;
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let state = &*state_ptr;
    match message {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() {
                let mut client: RECT = zeroed();
                GetClientRect(hwnd, &mut client);
                // GDI 和 GDI+ 共用零原点缓冲，目标 DC 保留 BeginPaint 的失效区剪裁
                if let Some(buffer) = ui_theme::PaintBuffer::new(dc, client) {
                    state.draw(buffer.dc(), &client);
                    if !buffer.present(dc) {
                        state.draw(dc, &paint.rcPaint);
                    }
                } else {
                    state.draw(dc, &paint.rcPaint);
                }
            }
            EndPaint(hwnd, &paint);
            0
        }
        WM_SIZE => {
            state.sync_metrics();
            0
        }
        WM_DPICHANGED_AFTERPARENT => {
            if state.set_dpi(GetDpiForWindow(hwnd)) {
                state.end_drag();
            }
            state.sync_metrics();
            state.request_layout();
            0
        }
        WM_VSCROLL if lparam == 0 => {
            state.vertical_scroll((wparam & 0xffff) as i32);
            0
        }
        WM_MOUSEWHEEL => {
            state.mouse_wheel(wparam);
            0
        }
        WM_MOUSEMOVE => {
            state.mouse_move(
                lparam as u16 as i16 as i32,
                (lparam >> 16) as u16 as i16 as i32,
            );
            0
        }
        WM_MOUSELEAVE => {
            state.tracking_mouse.set(false);
            state.set_hovered(false);
            0
        }
        WM_LBUTTONDOWN => {
            if state.mouse_down(
                lparam as u16 as i16 as i32,
                (lparam >> 16) as u16 as i16 as i32,
            ) {
                0
            } else {
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        WM_LBUTTONUP => {
            if state.drag_anchor.get().is_some() {
                state.mouse_move(
                    lparam as u16 as i16 as i32,
                    (lparam >> 16) as u16 as i16 as i32,
                );
            }
            state.end_drag();
            state.set_hovered(state.scrollbar_hit(
                lparam as u16 as i16 as i32,
                (lparam >> 16) as u16 as i16 as i32,
            ));
            0
        }
        WM_CAPTURECHANGED => {
            state.drag_anchor.set(None);
            state.set_hovered(false);
            state.invalidate_scrollbar();
            0
        }
        WM_CANCELMODE => {
            state.end_drag();
            state.set_hovered(false);
            0
        }
        WM_SHOWWINDOW if wparam == 0 => {
            state.end_drag();
            state.set_hovered(false);
            state.tracking_mouse.set(false);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        LAYOUT_NOTICE => {
            state.notice_pending.set(false);
            PostMessageW(state.parent, SCROLL_CHANGED, 0, 0);
            0
        }
        WM_COMMAND | WM_NOTIFY | WM_DRAWITEM | WM_MEASUREITEM | WM_CTLCOLORSTATIC
        | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX | WM_CTLCOLORSCROLLBAR
        | WM_CTLCOLORDLG | WM_CTLCOLORMSGBOX | WM_HSCROLL | WM_VSCROLL => {
            SendMessageW(state.parent, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            state.end_drag();
            state.hwnd.set(null_mut());
            state.notice_pending.set(false);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(child_proc), CHILD_SUBCLASS);
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }
    let state = &*(data as *const PanelState);
    if message == WM_MOUSEWHEEL {
        // 每次只交给容器处理，避免原生过程继续向父窗口冒泡而滚动两次
        state.mouse_wheel(wparam);
        return 0;
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if message == WM_SETFOCUS {
        state.ensure_visible(hwnd);
    }
    result
}
