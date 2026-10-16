use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use crate::localization::tr;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus as gdip;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const fn rgb(red: u32, green: u32, blue: u32) -> COLORREF {
    red | (green << 8) | (blue << 16)
}

pub const BACKGROUND: COLORREF = rgb(255, 255, 255);
pub const SURFACE: COLORREF = rgb(255, 255, 255);
pub const TEXT: COLORREF = rgb(36, 46, 63);
pub const MUTED: COLORREF = rgb(115, 127, 146);
pub const ACCENT: COLORREF = rgb(38, 108, 217);
pub const ACCENT_SOFT: COLORREF = rgb(233, 241, 255);
pub const BORDER: COLORREF = rgb(219, 226, 236);

pub const DISABLED_TEXT: COLORREF = rgb(166, 175, 190);
const HOVER: COLORREF = rgb(239, 244, 252);
const PRESSED: COLORREF = rgb(224, 234, 249);
const ACCENT_HOVER: COLORREF = rgb(30, 94, 198);
const ACCENT_PRESSED: COLORREF = rgb(24, 78, 170);
const PICKER_BORDER: COLORREF = rgb(189, 211, 245);
const PICKER_HOVER: COLORREF = rgb(219, 233, 255);
const TRACK: COLORREF = rgb(224, 231, 241);
const SUBTLE: COLORREF = rgb(248, 250, 253);
const BUTTON_HOVER_SUBCLASS: usize = 0x5452_5342;
const COMBO_THEME_SUBCLASS: usize = 0x5452_5343;
const COMBO_EDIT_THEME_SUBCLASS: usize = 0x5452_5345;
const INPUT_THEME_SUBCLASS: usize = 0x5452_5348;
const HOTKEY_TEXT_SUBCLASS: usize = 0x5452_534b;
const COMPACT_CHECKBOX_SUBCLASS: usize = 0x5452_5354;

thread_local! {
    static HIDDEN_WINDOWS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    static GDIPLUS_READY: Cell<bool> = const { Cell::new(false) };
}

// 在窗口与消息循环外持有会话，避免在线程局部存储析构时关闭 GDI+
pub struct GdiPlusSession {
    token: usize,
    _thread: PhantomData<*mut ()>,
}

impl GdiPlusSession {
    pub fn new() -> Option<Self> {
        let input = gdip::GdiplusStartupInput {
            GdiplusVersion: 1,
            SuppressExternalCodecs: 1,
            ..Default::default()
        };
        let mut token = 0;
        let status = unsafe { gdip::GdiplusStartup(&mut token, &input, null_mut()) };
        if status != gdip::Ok || token == 0 {
            return None;
        }
        GDIPLUS_READY.with(|ready| ready.set(true));
        Some(Self {
            token,
            _thread: PhantomData,
        })
    }
}

impl Drop for GdiPlusSession {
    fn drop(&mut self) {
        GDIPLUS_READY.with(|ready| ready.set(false));
        unsafe { gdip::GdiplusShutdown(self.token) };
    }
}

struct GdiPlusGraphics(*mut gdip::GpGraphics);

impl GdiPlusGraphics {
    unsafe fn new(hdc: HDC) -> Option<Self> {
        let mut graphics = Self(null_mut());
        let status = gdip::GdipCreateFromHDC(hdc, &mut graphics.0);
        (status == gdip::Ok && !graphics.0.is_null()).then_some(graphics)
    }
}

impl Drop for GdiPlusGraphics {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { gdip::GdipDeleteGraphics(self.0) };
        }
    }
}

struct GdiPlusPath(*mut gdip::GpPath);

impl GdiPlusPath {
    unsafe fn new() -> Option<Self> {
        let mut path = Self(null_mut());
        let status = gdip::GdipCreatePath(gdip::FillModeAlternate, &mut path.0);
        (status == gdip::Ok && !path.0.is_null()).then_some(path)
    }
}

impl Drop for GdiPlusPath {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { gdip::GdipDeletePath(self.0) };
        }
    }
}

struct GdiPlusPen(*mut gdip::GpPen);

impl GdiPlusPen {
    unsafe fn new(color: COLORREF, width: f32) -> Option<Self> {
        let argb = 0xff00_0000 | ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff);
        let mut pen = Self(null_mut());
        let status = gdip::GdipCreatePen1(argb, width, gdip::UnitPixel, &mut pen.0);
        (status == gdip::Ok && !pen.0.is_null()).then_some(pen)
    }
}

impl Drop for GdiPlusPen {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { gdip::GdipDeletePen(self.0) };
        }
    }
}

struct GdiPlusBrush(*mut gdip::GpSolidFill);

impl GdiPlusBrush {
    unsafe fn new(color: COLORREF) -> Option<Self> {
        let argb = 0xff00_0000 | ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff);
        let mut brush = Self(null_mut());
        let status = gdip::GdipCreateSolidFill(argb, &mut brush.0);
        (status == gdip::Ok && !brush.0.is_null()).then_some(brush)
    }
}

impl Drop for GdiPlusBrush {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { gdip::GdipDeleteBrush(self.0.cast()) };
        }
    }
}

pub fn set_hidden_windows(windows: impl IntoIterator<Item = usize>) {
    let windows = windows.into_iter().collect();
    HIDDEN_WINDOWS.with(|hidden| *hidden.borrow_mut() = windows);
}

fn hidden_window(window: usize) -> bool {
    HIDDEN_WINDOWS.with(|hidden| hidden.borrow().contains(&window))
}

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Picker,
    Refresh,
    Back,
    Primary,
    Secondary,
    Icon,
}

pub struct PaintBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    rect: RECT,
}

impl PaintBuffer {
    pub unsafe fn new(target: HDC, rect: RECT) -> Option<Self> {
        let width = rect.right.checked_sub(rect.left)?;
        let height = rect.bottom.checked_sub(rect.top)?;
        if target.is_null() || width <= 0 || height <= 0 {
            return None;
        }
        let dc = CreateCompatibleDC(target);
        if dc.is_null() {
            return None;
        }
        let bitmap = CreateCompatibleBitmap(target, width, height);
        if bitmap.is_null() {
            DeleteDC(dc);
            return None;
        }
        let previous_bitmap = SelectObject(dc, bitmap);
        if previous_bitmap.is_null() {
            DeleteObject(bitmap);
            DeleteDC(dc);
            return None;
        }
        let buffer = Self {
            dc,
            bitmap,
            previous_bitmap,
            rect,
        };
        // 保留调用方的逻辑坐标，支持左上角不为零的绘制区域
        if SetWindowOrgEx(dc, rect.left, rect.top, null_mut()) == 0 {
            return None;
        }
        Some(buffer)
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    pub unsafe fn present(&self, target: HDC) -> bool {
        BitBlt(
            target,
            self.rect.left,
            self.rect.top,
            self.rect.right - self.rect.left,
            self.rect.bottom - self.rect.top,
            self.dc,
            self.rect.left,
            self.rect.top,
            SRCCOPY,
        ) != 0
    }
}

impl Drop for PaintBuffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous_bitmap);
            DeleteDC(self.dc);
            DeleteObject(self.bitmap);
        }
    }
}

struct SavedDc {
    hdc: HDC,
    level: i32,
}

impl SavedDc {
    unsafe fn new(hdc: HDC) -> Option<Self> {
        let level = SaveDC(hdc);
        (level != 0).then_some(Self { hdc, level })
    }
}

impl Drop for SavedDc {
    fn drop(&mut self) {
        unsafe { RestoreDC(self.hdc, self.level) };
    }
}

fn pixels(dip: i32, dpi: u32) -> i32 {
    ((dip as i64 * dpi.max(96) as i64 + 48) / 96) as i32
}

unsafe fn fill(hdc: HDC, rect: &RECT, color: COLORREF) {
    SetDCBrushColor(hdc, color);
    FillRect(hdc, rect, GetStockObject(DC_BRUSH));
}

unsafe fn rounded(hdc: HDC, rect: &RECT, radius: i32, color: COLORREF) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    SetDCBrushColor(hdc, color);
    SelectObject(hdc, GetStockObject(DC_BRUSH));
    SelectObject(hdc, GetStockObject(NULL_PEN));
    RoundRect(
        hdc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius * 2,
        radius * 2,
    );
}

fn inset(rect: RECT, amount: i32) -> RECT {
    RECT {
        left: rect.left + amount,
        top: rect.top + amount,
        right: rect.right - amount,
        bottom: rect.bottom - amount,
    }
}

unsafe fn font_for(hdc: HDC, hwnd: HWND) {
    let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
    if !font.is_null() {
        SelectObject(hdc, font);
    }
}

unsafe fn hovered(hwnd: HWND) -> bool {
    let mut data = 0;
    GetWindowSubclass(
        hwnd,
        Some(button_hover_proc),
        BUTTON_HOVER_SUBCLASS,
        &mut data,
    ) != 0
        && data != 0
}

pub unsafe fn install_button_hover(hwnd: HWND) -> bool {
    SetWindowSubclass(hwnd, Some(button_hover_proc), BUTTON_HOVER_SUBCLASS, 0) != 0
}

unsafe extern "system" fn button_hover_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND
            if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & BS_TYPEMASK as u32
                == BS_OWNERDRAW as u32 =>
        {
            // 自绘按钮会完整填充矩形，避免默认擦底先显示灰块
            return 1;
        }
        WM_MOUSEMOVE if data == 0 => {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            if TrackMouseEvent(&mut tracking) != 0 {
                SetWindowSubclass(hwnd, Some(button_hover_proc), subclass, 1);
                InvalidateRect(hwnd, null(), 0);
            }
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(button_hover_proc), subclass, 0);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(button_hover_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_UPDATEUISTATE
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}

pub unsafe fn install_compact_checkbox(hwnd: HWND) -> bool {
    SetWindowSubclass(
        hwnd,
        Some(compact_checkbox_proc),
        COMPACT_CHECKBOX_SUBCLASS,
        0,
    ) != 0
}

unsafe extern "system" fn compact_checkbox_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            paint_compact_checkbox(hwnd, dc, data != 0);
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_PRINTCLIENT => {
            paint_compact_checkbox(hwnd, wparam as HDC, data != 0);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_MOUSEMOVE if data == 0 => {
            track_combo_hover(hwnd);
            SetWindowSubclass(hwnd, Some(compact_checkbox_proc), subclass, 1);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(compact_checkbox_proc), subclass, 0);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(compact_checkbox_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_ENABLE
            | WM_SETFOCUS
            | WM_KILLFOCUS
            | WM_UPDATEUISTATE
            | WM_SETTEXT
            | WM_SETFONT
            | BM_SETCHECK
            | BM_SETSTATE
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_KEYDOWN
            | WM_KEYUP
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}

unsafe fn paint_compact_checkbox(hwnd: HWND, dc: HDC, hovered: bool) {
    let mut rect: RECT = zeroed();
    if dc.is_null() || GetClientRect(hwnd, &mut rect) == 0 {
        return;
    }
    if let Some(buffer) = PaintBuffer::new(dc, rect) {
        if draw_compact_checkbox(hwnd, buffer.dc(), rect, hovered) && buffer.present(dc) {
            return;
        }
    }
    draw_compact_checkbox(hwnd, dc, rect, hovered);
}

unsafe fn draw_compact_checkbox(hwnd: HWND, dc: HDC, rect: RECT, hovered: bool) -> bool {
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) != BST_UNCHECKED as isize;
    let pressed = SendMessageW(hwnd, BM_GETSTATE, 0, 0) as u32 & BST_PUSHED != 0;
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0_u16; length + 1];
    let copied = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
    font_for(dc, hwnd);
    let mut size: SIZE = zeroed();
    GetTextExtentPoint32W(dc, text.as_ptr(), copied, &mut size);
    let side = pixels(12, dpi);
    let gap = pixels(4, dpi);
    let text_left = (rect.right - size.cx).max(rect.left + side + gap);
    let mut label = RECT {
        left: text_left,
        ..rect
    };
    let glyph = RECT {
        left: text_left - gap - side,
        top: rect.top + (rect.bottom - rect.top - side) / 2,
        right: text_left - gap,
        bottom: rect.top + (rect.bottom - rect.top - side) / 2 + side,
    };
    let border = if !enabled {
        BORDER
    } else if checked || pressed {
        ACCENT
    } else if hovered {
        PICKER_BORDER
    } else {
        MUTED
    };
    let face = if !enabled {
        SUBTLE
    } else if pressed {
        PRESSED
    } else if checked {
        ACCENT_SOFT
    } else if hovered {
        HOVER
    } else {
        SURFACE
    };
    let foreground = if enabled { ACCENT } else { DISABLED_TEXT };
    fill(dc, &rect, BACKGROUND);
    if !checkbox_glyph_antialiased(dc, glyph, dpi, border, face, foreground, checked) {
        rounded(dc, &glyph, pixels(3, dpi), border);
        rounded(dc, &inset(glyph, pixels(1, dpi)), pixels(2, dpi), face);
        if checked {
            let pen = CreatePen(PS_SOLID, pixels(1, dpi), foreground);
            if !pen.is_null() {
                let previous = SelectObject(dc, pen);
                let points = [
                    POINT {
                        x: glyph.left + pixels(3, dpi),
                        y: glyph.top + pixels(6, dpi),
                    },
                    POINT {
                        x: glyph.left + pixels(5, dpi),
                        y: glyph.top + pixels(8, dpi),
                    },
                    POINT {
                        x: glyph.left + pixels(9, dpi),
                        y: glyph.top + pixels(4, dpi),
                    },
                ];
                Polyline(dc, points.as_ptr(), points.len() as i32);
                SelectObject(dc, previous);
                DeleteObject(pen);
            }
        }
    }
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, if enabled { TEXT } else { DISABLED_TEXT });
    DrawTextW(
        dc,
        text.as_ptr(),
        copied,
        &mut label,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    if GetFocus() == hwnd && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) as u32 & UISF_HIDEFOCUS == 0
    {
        DrawFocusRect(dc, &inset(rect, pixels(1, dpi)));
    }
    true
}

unsafe fn checkbox_glyph_antialiased(
    dc: HDC,
    rect: RECT,
    dpi: u32,
    border: COLORREF,
    background: COLORREF,
    foreground: COLORREF,
    checked: bool,
) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(dc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let scale = dpi.max(96) as f32 / 96.0;
    let Some(pen) = GdiPlusPen::new(border, scale) else {
        return false;
    };
    let Some(brush) = GdiPlusBrush::new(background) else {
        return false;
    };
    if gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) != gdip::Ok
        || gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) != gdip::Ok
        || gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) != gdip::Ok
        || !rounded_path(
            &path,
            rect.left as f32 + 0.5 * scale,
            rect.top as f32 + 0.5 * scale,
            rect.right as f32 - 0.5 * scale,
            rect.bottom as f32 - 0.5 * scale,
            5.0 * scale,
        )
        || gdip::GdipFillPath(graphics.0, brush.0.cast(), path.0) != gdip::Ok
        || gdip::GdipDrawPath(graphics.0, pen.0, path.0) != gdip::Ok
    {
        return false;
    }
    if checked {
        let Some(check_pen) = GdiPlusPen::new(foreground, 1.5 * scale) else {
            return false;
        };
        let points = [
            gdip::PointF {
                X: rect.left as f32 + 3.0 * scale,
                Y: rect.top as f32 + 6.0 * scale,
            },
            gdip::PointF {
                X: rect.left as f32 + 5.0 * scale,
                Y: rect.top as f32 + 8.0 * scale,
            },
            gdip::PointF {
                X: rect.left as f32 + 9.0 * scale,
                Y: rect.top as f32 + 4.0 * scale,
            },
        ];
        if gdip::GdipSetPenStartCap(check_pen.0, gdip::LineCapRound) != gdip::Ok
            || gdip::GdipSetPenEndCap(check_pen.0, gdip::LineCapRound) != gdip::Ok
            || gdip::GdipSetPenLineJoin(check_pen.0, gdip::LineJoinRound) != gdip::Ok
            || gdip::GdipDrawLines(
                graphics.0,
                check_pen.0,
                points.as_ptr(),
                points.len() as i32,
            ) != gdip::Ok
        {
            return false;
        }
    }
    gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

pub unsafe fn install_combo_theme(combo: HWND, edit: HWND) -> bool {
    let empty = [0_u16];
    SetWindowTheme(combo, empty.as_ptr(), empty.as_ptr());
    SetWindowTheme(edit, empty.as_ptr(), empty.as_ptr());
    let style = GetWindowLongPtrW(combo, GWL_STYLE) as u32;
    SetWindowLongPtrW(combo, GWL_STYLE, (style | WS_CLIPCHILDREN) as isize);
    if SetWindowSubclass(combo, Some(combo_theme_proc), COMBO_THEME_SUBCLASS, 0) == 0 {
        return false;
    }
    if SetWindowSubclass(
        edit,
        Some(combo_edit_theme_proc),
        COMBO_EDIT_THEME_SUBCLASS,
        0,
    ) == 0
    {
        RemoveWindowSubclass(combo, Some(combo_theme_proc), COMBO_THEME_SUBCLASS);
        return false;
    }
    true
}

pub unsafe fn install_dropdown_theme(combo: HWND) -> bool {
    let empty = [0_u16];
    SetWindowTheme(combo, empty.as_ptr(), empty.as_ptr());
    SetWindowSubclass(combo, Some(combo_theme_proc), COMBO_THEME_SUBCLASS, 0) != 0
}

pub unsafe fn install_hotkey_theme(hwnd: HWND) -> bool {
    if !install_input_theme(hwnd) {
        return false;
    }
    if SetWindowSubclass(hwnd, Some(hotkey_text_proc), HOTKEY_TEXT_SUBCLASS, 0) == 0 {
        RemoveWindowSubclass(hwnd, Some(input_theme_proc), INPUT_THEME_SUBCLASS);
        return false;
    }
    true
}

pub unsafe fn install_input_theme(hwnd: HWND) -> bool {
    let empty = [0_u16];
    SetWindowTheme(hwnd, empty.as_ptr(), empty.as_ptr());
    SetWindowSubclass(hwnd, Some(input_theme_proc), INPUT_THEME_SUBCLASS, 0) != 0
}

unsafe fn is_edit_control(hwnd: HWND) -> bool {
    let mut class = [0_u16; 16];
    let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    length == 4
        && class[..4]
            .iter()
            .copied()
            .zip([b'E', b'D', b'I', b'T'])
            .all(|(actual, expected)| actual == expected as u16 || actual == (expected + 32) as u16)
}

unsafe fn input_content_rect(hwnd: HWND, mut rect: RECT) -> RECT {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let dc = GetDC(hwnd);
    let mut text_height = pixels(18, dpi);
    if !dc.is_null() {
        if let Some(_saved) = SavedDc::new(dc) {
            font_for(dc, hwnd);
            let mut metrics: TEXTMETRICW = zeroed();
            if GetTextMetricsW(dc, &mut metrics) != 0 {
                text_height = metrics.tmHeight;
            }
        }
        ReleaseDC(hwnd, dc);
    }
    let height = (rect.bottom - rect.top).max(0);
    let edit = is_edit_control(hwnd);
    let line_height = (text_height + if edit { 0 } else { pixels(2, dpi) }).min(height);
    let top = rect.top + (height - line_height) / 2;
    let horizontal =
        pixels(if edit { 6 } else { 9 }, dpi).min(((rect.right - rect.left) / 2).max(0));
    rect.left += horizontal;
    rect.right -= horizontal;
    rect.top = top;
    rect.bottom = top + line_height;
    rect
}

pub unsafe fn update_hotkey_metrics(hwnd: HWND) {
    update_input_metrics(hwnd);
}

pub unsafe fn update_input_metrics(hwnd: HWND) {
    if is_edit_control(hwnd) && SendMessageW(hwnd, EM_GETMARGINS, 0, 0) != 0 {
        // 外侧内边距已计入客户区，清除单行输入框原生的左右边距
        SendMessageW(
            hwnd,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            0,
        );
    }
    let mut outer: RECT = zeroed();
    let mut client: RECT = zeroed();
    let mut origin: POINT = zeroed();
    if GetWindowRect(hwnd, &mut outer) == 0
        || GetClientRect(hwnd, &mut client) == 0
        || ClientToScreen(hwnd, &mut origin) == 0
    {
        return;
    }
    let expected = input_content_rect(
        hwnd,
        RECT {
            right: outer.right - outer.left,
            bottom: outer.bottom - outer.top,
            ..zeroed()
        },
    );
    let left = origin.x - outer.left;
    let top = origin.y - outer.top;
    if left == expected.left
        && top == expected.top
        && client.right == expected.right - expected.left
        && client.bottom == expected.bottom - expected.top
    {
        return;
    }
    // 只重新计算客户区，保留父窗口批量布局后的统一重绘
    SetWindowPos(
        hwnd,
        null_mut(),
        0,
        0,
        0,
        0,
        SWP_NOMOVE
            | SWP_NOSIZE
            | SWP_NOZORDER
            | SWP_NOACTIVATE
            | SWP_FRAMECHANGED
            | SWP_NOREDRAW
            | SWP_NOCOPYBITS,
    );
}

unsafe extern "system" fn hotkey_text_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_GETTEXTLENGTH if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            return tr("无", "None").encode_utf16().count() as isize;
        }
        WM_GETTEXT if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            if wparam == 0 || lparam == 0 {
                return 0;
            }
            let text: Vec<u16> = tr("无", "None").encode_utf16().collect();
            let copied = text.len().min(wparam - 1);
            let destination = lparam as *mut u16;
            std::ptr::copy_nonoverlapping(text.as_ptr(), destination, copied);
            *destination.add(copied) = 0;
            return copied as isize;
        }
        WM_PAINT if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            paint_empty_hotkey(hwnd, dc, true);
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_PRINTCLIENT if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            paint_empty_hotkey(hwnd, wparam as HDC, false);
            return 0;
        }
        WM_ERASEBKGND if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            let mut rect: RECT = zeroed();
            if GetClientRect(hwnd, &mut rect) != 0 {
                fill(wparam as HDC, &rect, SURFACE);
            }
            return 1;
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(hotkey_text_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        HKM_SETHOTKEY
            | WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_SETFOCUS
            | WM_KILLFOCUS
            | WM_ENABLE
            | WM_SETFONT
            | WM_THEMECHANGED
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}

unsafe fn paint_empty_hotkey(hwnd: HWND, dc: HDC, update_caret: bool) {
    let mut rect: RECT = zeroed();
    if dc.is_null() || GetClientRect(hwnd, &mut rect) == 0 {
        return;
    }
    let Some(_saved) = SavedDc::new(dc) else {
        return;
    };
    // 空值说明由应用语言决定，组合键内容与输入行为仍由原生控件管理
    fill(dc, &rect, SURFACE);
    font_for(dc, hwnd);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(
        dc,
        if IsWindowEnabled(hwnd) == 0 {
            DISABLED_TEXT
        } else {
            TEXT
        },
    );
    let text: Vec<u16> = tr("无", "None").encode_utf16().collect();
    DrawTextW(
        dc,
        text.as_ptr(),
        text.len() as i32,
        &mut rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    if update_caret && GetFocus() == hwnd && IsWindowEnabled(hwnd) != 0 {
        let mut extent: SIZE = zeroed();
        if GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32, &mut extent) != 0 {
            // 只调整原生光标的位置，保留其创建、闪烁与键盘更新机制
            SetCaretPos(
                extent.cx.min((rect.right - 1).max(0)),
                ((rect.bottom - extent.cy) / 2).max(0),
            );
        }
    }
}

unsafe extern "system" fn input_theme_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_NCCALCSIZE if lparam != 0 => {
            let outer = if wparam != 0 {
                (*(lparam as *mut NCCALCSIZE_PARAMS)).rgrc[0]
            } else {
                *(lparam as *mut RECT)
            };
            DefSubclassProc(hwnd, message, wparam, lparam);
            let rect = if wparam != 0 {
                &mut (*(lparam as *mut NCCALCSIZE_PARAMS)).rgrc[0]
            } else {
                &mut *(lparam as *mut RECT)
            };
            // 每次从完整窗口计算客户区，原生文字和光标保持垂直居中
            *rect = input_content_rect(hwnd, outer);
            return 0;
        }
        WM_NCPAINT => {
            paint_input_frame(hwnd);
            return 0;
        }
        WM_NCHITTEST => return HTCLIENT as isize,
        WM_MOUSEMOVE if data == 0 => {
            track_combo_hover(hwnd);
            SetWindowSubclass(hwnd, Some(input_theme_proc), subclass, 1);
            RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(input_theme_proc), subclass, 0);
            RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(input_theme_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if message == WM_SETFONT {
        update_input_metrics(hwnd);
    }
    if matches!(
        message,
        WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE | WM_THEMECHANGED
    ) {
        RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
    }
    result
}

unsafe fn paint_input_frame(hwnd: HWND) {
    let mut outer: RECT = zeroed();
    let mut client: RECT = zeroed();
    let mut origin: POINT = zeroed();
    if GetWindowRect(hwnd, &mut outer) == 0
        || GetClientRect(hwnd, &mut client) == 0
        || ClientToScreen(hwnd, &mut origin) == 0
    {
        return;
    }
    let dc = GetWindowDC(hwnd);
    if dc.is_null() {
        return;
    }
    if let Some(_saved) = SavedDc::new(dc) {
        let rect = RECT {
            right: outer.right - outer.left,
            bottom: outer.bottom - outer.top,
            ..zeroed()
        };
        let left = origin.x - outer.left;
        let top = origin.y - outer.top;
        // 框体重绘不能覆盖原生输入文字或闪烁光标
        ExcludeClipRect(dc, left, top, left + client.right, top + client.bottom);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let enabled = IsWindowEnabled(hwnd) != 0;
        let mut hover = 0;
        GetWindowSubclass(
            hwnd,
            Some(input_theme_proc),
            INPUT_THEME_SUBCLASS,
            &mut hover,
        );
        let border = if enabled && GetFocus() == hwnd {
            ACCENT
        } else if enabled && hover != 0 {
            PICKER_BORDER
        } else {
            BORDER
        };
        fill(dc, &rect, BACKGROUND);
        if !draw_input_outline_antialiased(dc, rect, dpi, border) {
            let outline = inset(rect, pixels(1, dpi));
            rounded(dc, &outline, pixels(7, dpi), border);
            rounded(dc, &inset(outline, pixels(1, dpi)), pixels(6, dpi), SURFACE);
        }
    }
    ReleaseDC(hwnd, dc);
}

pub unsafe fn update_combo_metrics(combo: HWND, dpi: u32) {
    SendMessageW(combo, CB_SETITEMHEIGHT, 0, pixels(26, dpi) as isize);
    let target = pixels(32, dpi);
    // 下拉框的总高度还包含候选列表，仅校准闭合后的选择区高度
    for _ in 0..2 {
        let selection = SendMessageW(combo, CB_GETITEMHEIGHT, usize::MAX, 0);
        let mut rect: RECT = zeroed();
        if selection < 0 || GetWindowRect(combo, &mut rect) == 0 {
            break;
        }
        let actual = rect.bottom - rect.top;
        if actual == target {
            break;
        }
        let height = if actual > 0 {
            (selection as i32 + target - actual).max(pixels(18, dpi))
        } else {
            target - pixels(6, dpi)
        };
        if SendMessageW(combo, CB_SETITEMHEIGHT, usize::MAX, height as isize) == CB_ERR as isize {
            break;
        }
    }
    position_combo_edit(combo);
}

unsafe fn combo_info(combo: HWND) -> Option<COMBOBOXINFO> {
    let mut info = COMBOBOXINFO {
        cbSize: size_of::<COMBOBOXINFO>() as u32,
        ..zeroed()
    };
    (GetComboBoxInfo(combo, &mut info) != 0).then_some(info)
}

unsafe fn position_combo_edit(combo: HWND) {
    if GetWindowLongPtrW(combo, GWL_STYLE) as u32 & 0x3 == CBS_DROPDOWNLIST as u32 {
        return;
    }
    let Some(info) = combo_info(combo) else {
        return;
    };
    let mut client: RECT = zeroed();
    if info.hwndItem.is_null() || GetClientRect(combo, &mut client) == 0 {
        return;
    }
    let dpi = GetDpiForWindow(combo).max(96);
    let dc = GetDC(info.hwndItem);
    if dc.is_null() {
        return;
    }
    let font = SendMessageW(info.hwndItem, WM_GETFONT, 0, 0) as HFONT;
    let old_font = if font.is_null() {
        null_mut()
    } else {
        SelectObject(dc, font)
    };
    let mut metrics: TEXTMETRICW = zeroed();
    let measured = GetTextMetricsW(dc, &mut metrics) != 0;
    if !old_font.is_null() {
        SelectObject(dc, old_font);
    }
    ReleaseDC(info.hwndItem, dc);
    if !measured || client.bottom <= pixels(12, dpi) {
        return;
    }
    let height = (metrics.tmHeight + pixels(2, dpi)).min(client.bottom - pixels(8, dpi));
    let bounds = RECT {
        left: pixels(9, dpi),
        top: (client.bottom - height) / 2,
        right: (info.rcButton.left - pixels(6, dpi)).max(pixels(10, dpi)),
        bottom: (client.bottom - height) / 2 + height,
    };
    let mut previous: RECT = zeroed();
    if GetWindowRect(info.hwndItem, &mut previous) != 0 {
        MapWindowPoints(
            null_mut(),
            combo,
            &mut previous as *mut RECT as *mut POINT,
            2,
        );
        if previous.left == bounds.left
            && previous.top == bounds.top
            && previous.right == bounds.right
            && previous.bottom == bounds.bottom
        {
            return;
        }
    }
    SendMessageW(
        info.hwndItem,
        EM_SETMARGINS,
        (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
        0,
    );
    SetWindowPos(
        info.hwndItem,
        null_mut(),
        bounds.left,
        bounds.top,
        bounds.right - bounds.left,
        height,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS,
    );
}

unsafe fn track_combo_hover(hwnd: HWND) {
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    TrackMouseEvent(&mut tracking);
}

unsafe extern "system" fn combo_edit_theme_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE if data == 0 => {
            track_combo_hover(hwnd);
            SetWindowSubclass(hwnd, Some(combo_edit_theme_proc), subclass, 1);
            InvalidateRect(GetParent(hwnd), null(), 0);
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(combo_edit_theme_proc), subclass, 0);
            InvalidateRect(GetParent(hwnd), null(), 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(combo_edit_theme_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(message, WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE) {
        InvalidateRect(GetParent(hwnd), null(), 0);
    }
    result
}

unsafe extern "system" fn combo_theme_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            paint_combo(hwnd, dc);
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_PRINTCLIENT => {
            paint_combo(hwnd, wparam as HDC);
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_NCPAINT => return 0,
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            SetTextColor(wparam as HDC, TEXT);
            SetBkColor(wparam as HDC, SURFACE);
            return GetStockObject(WHITE_BRUSH) as isize;
        }
        WM_MOUSEMOVE if data == 0 => {
            track_combo_hover(hwnd);
            SetWindowSubclass(hwnd, Some(combo_theme_proc), subclass, 1);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_MOUSELEAVE => {
            SetWindowSubclass(hwnd, Some(combo_theme_proc), subclass, 0);
            InvalidateRect(hwnd, null(), 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(combo_theme_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(message, WM_SIZE | WM_SETFONT | CB_SETITEMHEIGHT) {
        position_combo_edit(hwnd);
    }
    if matches!(
        message,
        WM_ENABLE
            | WM_SETFOCUS
            | WM_KILLFOCUS
            | WM_THEMECHANGED
            | WM_SETTEXT
            | CB_RESETCONTENT
            | CB_SETCURSEL
            | CB_SELECTSTRING
            | CB_SHOWDROPDOWN
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}

unsafe fn paint_combo(hwnd: HWND, dc: HDC) {
    let mut rect: RECT = zeroed();
    if dc.is_null() || GetClientRect(hwnd, &mut rect) == 0 {
        return;
    }
    if let Some(buffer) = PaintBuffer::new(dc, rect) {
        if draw_combo_frame(hwnd, buffer.dc(), rect) && buffer.present(dc) {
            return;
        }
    }
    draw_combo_frame(hwnd, dc, rect);
}

unsafe fn draw_combo_frame(hwnd: HWND, dc: HDC, rect: RECT) -> bool {
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    let info = combo_info(hwnd);
    let focused = GetFocus() == hwnd
        || info
            .as_ref()
            .is_some_and(|info| GetFocus() == info.hwndItem);
    let dropped = SendMessageW(hwnd, CB_GETDROPPEDSTATE, 0, 0) != 0;
    let mut cursor: POINT = zeroed();
    let hovered = GetCursorPos(&mut cursor) != 0 && {
        ScreenToClient(hwnd, &mut cursor);
        cursor.x >= rect.left
            && cursor.x < rect.right
            && cursor.y >= rect.top
            && cursor.y < rect.bottom
    };
    let disabled = IsWindowEnabled(hwnd) == 0;
    let border = if !disabled && (focused || dropped) {
        ACCENT
    } else if !disabled && hovered {
        PICKER_BORDER
    } else {
        BORDER
    };
    let foreground = if disabled {
        DISABLED_TEXT
    } else if focused || dropped {
        ACCENT
    } else {
        MUTED
    };
    fill(dc, &rect, BACKGROUND);
    let button = info.map(|info| info.rcButton).unwrap_or(RECT {
        left: rect.right - pixels(24, dpi),
        ..rect
    });
    if !draw_combo_antialiased(dc, rect, button, dpi, border, foreground) {
        let outline = inset(rect, pixels(1, dpi));
        rounded(dc, &outline, pixels(7, dpi), border);
        rounded(dc, &inset(outline, pixels(1, dpi)), pixels(6, dpi), SURFACE);
        let cx = (button.left + button.right) / 2;
        let cy = (rect.top + rect.bottom) / 2;
        let pen = CreatePen(PS_SOLID, pixels(1, dpi), foreground);
        if !pen.is_null() {
            let previous = SelectObject(dc, pen);
            let half = pixels(3, dpi);
            let points = [
                POINT {
                    x: cx - half,
                    y: cy - half / 2,
                },
                POINT {
                    x: cx,
                    y: cy + half / 2,
                },
                POINT {
                    x: cx + half,
                    y: cy - half / 2,
                },
            ];
            Polyline(dc, points.as_ptr(), points.len() as i32);
            SelectObject(dc, previous);
            DeleteObject(pen);
        }
    }
    if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & 0x3 == CBS_DROPDOWNLIST as u32 {
        let selection = SendMessageW(hwnd, CB_GETCURSEL, 0, 0);
        if selection == CB_ERR as isize {
            return true;
        }
        let length = SendMessageW(hwnd, CB_GETLBTEXTLEN, selection as usize, 0);
        if length < 0 {
            return true;
        }
        let mut text = vec![0_u16; length as usize + 1];
        let copied = SendMessageW(
            hwnd,
            CB_GETLBTEXT,
            selection as usize,
            text.as_mut_ptr() as isize,
        );
        if copied < 0 {
            return true;
        }
        font_for(dc, hwnd);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, if disabled { DISABLED_TEXT } else { TEXT });
        let mut content = RECT {
            left: rect.left + pixels(9, dpi),
            right: button.left - pixels(6, dpi),
            ..rect
        };
        DrawTextW(
            dc,
            text.as_ptr(),
            copied as i32,
            &mut content,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
    true
}

unsafe fn rounded_input_path(path: &GdiPlusPath, rect: RECT, scale: f32) -> bool {
    let left = rect.left as f32 + 1.5 * scale;
    let top = rect.top as f32 + 1.5 * scale;
    let right = rect.right as f32 - 1.5 * scale;
    let bottom = rect.bottom as f32 - 1.5 * scale;
    let diameter = 12.0 * scale;
    rounded_path(path, left, top, right, bottom, diameter)
}

unsafe fn rounded_path(
    path: &GdiPlusPath,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    diameter: f32,
) -> bool {
    right - left >= diameter
        && bottom - top >= diameter
        && gdip::GdipAddPathArc(path.0, left, top, diameter, diameter, 180.0, 90.0) == gdip::Ok
        && gdip::GdipAddPathArc(
            path.0,
            right - diameter,
            top,
            diameter,
            diameter,
            270.0,
            90.0,
        ) == gdip::Ok
        && gdip::GdipAddPathArc(
            path.0,
            right - diameter,
            bottom - diameter,
            diameter,
            diameter,
            0.0,
            90.0,
        ) == gdip::Ok
        && gdip::GdipAddPathArc(
            path.0,
            left,
            bottom - diameter,
            diameter,
            diameter,
            90.0,
            90.0,
        ) == gdip::Ok
        && gdip::GdipClosePathFigure(path.0) == gdip::Ok
}

pub unsafe fn draw_scrollbar(
    dc: HDC,
    track: RECT,
    thumb_top: i32,
    thumb_height: i32,
    dpi: u32,
    hovered: bool,
    dragging: bool,
) {
    if track.right <= track.left || track.bottom <= track.top {
        return;
    }
    let Some(_saved) = SavedDc::new(dc) else {
        return;
    };
    fill(dc, &track, BACKGROUND);
    let active = hovered || dragging;
    let center = track.left + (track.right - track.left) / 2;
    if active {
        let width = pixels(2, dpi).max(1).min(track.right - track.left);
        let rail = RECT {
            left: center - width / 2,
            right: center - width / 2 + width,
            ..track
        };
        draw_scrollbar_thumb(dc, &rail, ACCENT_SOFT);
    }
    let width = pixels(if active { 6 } else { 4 }, dpi).min(track.right - track.left);
    let top = thumb_top.clamp(track.top, track.bottom);
    let thumb = RECT {
        left: center - width / 2,
        top,
        right: center - width / 2 + width,
        bottom: top.saturating_add(thumb_height.max(0)).min(track.bottom),
    };
    let color = if dragging {
        ACCENT
    } else if hovered {
        MUTED
    } else {
        DISABLED_TEXT
    };
    draw_scrollbar_thumb(dc, &thumb, color);
}

unsafe fn draw_scrollbar_thumb(dc: HDC, rect: &RECT, color: COLORREF) {
    let diameter = (rect.right - rect.left).min(rect.bottom - rect.top);
    if diameter <= 0 || dc.is_null() {
        return;
    }
    if !scrollbar_thumb_antialiased(dc, rect, color, diameter as f32) {
        if let Some(_saved) = SavedDc::new(dc) {
            rounded(dc, rect, (diameter + 1) / 2, color);
        }
    }
}

unsafe fn scrollbar_thumb_antialiased(
    dc: HDC,
    rect: &RECT,
    color: COLORREF,
    diameter: f32,
) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(dc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let Some(brush) = GdiPlusBrush::new(color) else {
        return false;
    };
    gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) == gdip::Ok
        && gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) == gdip::Ok
        && gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) == gdip::Ok
        && rounded_path(
            &path,
            rect.left as f32,
            rect.top as f32,
            rect.right as f32,
            rect.bottom as f32,
            diameter,
        )
        && gdip::GdipFillPath(graphics.0, brush.0.cast(), path.0) == gdip::Ok
        && gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

unsafe fn draw_input_outline_antialiased(dc: HDC, rect: RECT, dpi: u32, border: COLORREF) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(dc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let scale = dpi.max(96) as f32 / 96.0;
    let Some(pen) = GdiPlusPen::new(border, scale) else {
        return false;
    };
    gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) == gdip::Ok
        && gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) == gdip::Ok
        && gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) == gdip::Ok
        && rounded_input_path(&path, rect, scale)
        && gdip::GdipDrawPath(graphics.0, pen.0, path.0) == gdip::Ok
        && gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

unsafe fn draw_combo_antialiased(
    dc: HDC,
    rect: RECT,
    button: RECT,
    dpi: u32,
    border: COLORREF,
    foreground: COLORREF,
) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(dc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let scale = dpi.max(96) as f32 / 96.0;
    let Some(border_pen) = GdiPlusPen::new(border, scale) else {
        return false;
    };
    let Some(arrow_pen) = GdiPlusPen::new(foreground, 1.5 * scale) else {
        return false;
    };
    if gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) != gdip::Ok
        || gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) != gdip::Ok
        || gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) != gdip::Ok
        || !rounded_input_path(&path, rect, scale)
        || gdip::GdipDrawPath(graphics.0, border_pen.0, path.0) != gdip::Ok
        || gdip::GdipSetPenStartCap(arrow_pen.0, gdip::LineCapRound) != gdip::Ok
        || gdip::GdipSetPenEndCap(arrow_pen.0, gdip::LineCapRound) != gdip::Ok
        || gdip::GdipSetPenLineJoin(arrow_pen.0, gdip::LineJoinRound) != gdip::Ok
    {
        return false;
    }
    let cx = (button.left + button.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let points = [
        gdip::PointF {
            X: cx - 3.5 * scale,
            Y: cy - 1.75 * scale,
        },
        gdip::PointF {
            X: cx,
            Y: cy + 1.75 * scale,
        },
        gdip::PointF {
            X: cx + 3.5 * scale,
            Y: cy - 1.75 * scale,
        },
    ];
    gdip::GdipDrawLines(
        graphics.0,
        arrow_pen.0,
        points.as_ptr(),
        points.len() as i32,
    ) == gdip::Ok
        && gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

pub unsafe fn draw_combo_item(item: &DRAWITEMSTRUCT) {
    if let Some(buffer) = PaintBuffer::new(item.hDC, item.rcItem) {
        let buffered = DRAWITEMSTRUCT {
            hDC: buffer.dc(),
            ..*item
        };
        if draw_combo_item_content(&buffered) && buffer.present(item.hDC) {
            return;
        }
    }
    draw_combo_item_content(item);
}

unsafe fn draw_combo_item_content(item: &DRAWITEMSTRUCT) -> bool {
    let Some(_saved) = SavedDc::new(item.hDC) else {
        return false;
    };
    let selected = item.itemState & ODS_SELECTED != 0;
    fill(
        item.hDC,
        &item.rcItem,
        if selected { ACCENT_SOFT } else { SURFACE },
    );
    if item.itemID == u32::MAX {
        return true;
    }
    let length = SendMessageW(item.hwndItem, CB_GETLBTEXTLEN, item.itemID as usize, 0);
    if length < 0 {
        return true;
    }
    let mut text = vec![0_u16; length as usize + 1];
    let copied = SendMessageW(
        item.hwndItem,
        CB_GETLBTEXT,
        item.itemID as usize,
        text.as_mut_ptr() as isize,
    );
    if copied < 0 {
        return true;
    }
    font_for(item.hDC, item.hwndItem);
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SetTextColor(item.hDC, if selected { ACCENT } else { TEXT });
    let mut content = item.rcItem;
    let margin = pixels(9, GetDpiForWindow(item.hwndItem).max(96));
    content.left += margin;
    content.right -= margin;
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        copied as i32,
        &mut content,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    true
}

unsafe fn back_icon(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    if back_icon_antialiased(hdc, rect, dpi, color) {
        return;
    }
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let arrow = [
        POINT {
            x: cx,
            y: cy - pixels(5, dpi),
        },
        POINT {
            x: cx - pixels(5, dpi),
            y: cy,
        },
        POINT {
            x: cx,
            y: cy + pixels(5, dpi),
        },
    ];
    let brush = LOGBRUSH {
        lbStyle: BS_SOLID,
        lbColor: color,
        lbHatch: 0,
    };
    let stroke = ((dpi.max(96) as u64 * 3 + 96) / 192) as u32;
    let pen = ExtCreatePen(
        (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32,
        stroke,
        &brush,
        0,
        null(),
    );
    if pen.is_null() {
        return;
    }
    let previous = SelectObject(hdc, pen);
    Polyline(hdc, arrow.as_ptr(), arrow.len() as i32);
    MoveToEx(hdc, cx - pixels(5, dpi), cy, null_mut());
    LineTo(hdc, cx + pixels(6, dpi), cy);
    SelectObject(hdc, previous);
    DeleteObject(pen);
}

unsafe fn back_icon_antialiased(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let mut corners = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    if LPtoDP(hdc, corners.as_mut_ptr(), corners.len() as i32) == 0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(hdc) else {
        return false;
    };
    if SetWindowOrgEx(hdc, 0, 0, null_mut()) == 0 || SetViewportOrgEx(hdc, 0, 0, null_mut()) == 0 {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(hdc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let scale = dpi.max(96) as f32 / 96.0;
    let Some(pen) = GdiPlusPen::new(color, 1.5 * scale) else {
        return false;
    };
    let cx = (corners[0].x as f32 + corners[1].x as f32) / 2.0;
    let cy = (corners[0].y as f32 + corners[1].y as f32) / 2.0;
    let arrow = [
        gdip::PointF {
            X: cx,
            Y: cy - 5.0 * scale,
        },
        gdip::PointF {
            X: cx - 5.0 * scale,
            Y: cy,
        },
        gdip::PointF {
            X: cx,
            Y: cy + 5.0 * scale,
        },
    ];
    gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) == gdip::Ok
        && gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) == gdip::Ok
        && gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) == gdip::Ok
        && gdip::GdipSetPenStartCap(pen.0, gdip::LineCapRound) == gdip::Ok
        && gdip::GdipSetPenEndCap(pen.0, gdip::LineCapRound) == gdip::Ok
        && gdip::GdipSetPenLineJoin(pen.0, gdip::LineJoinRound) == gdip::Ok
        && gdip::GdipAddPathLine2(path.0, arrow.as_ptr(), arrow.len() as i32) == gdip::Ok
        && gdip::GdipStartPathFigure(path.0) == gdip::Ok
        && gdip::GdipAddPathLine(path.0, cx - 5.0 * scale, cy, cx + 6.0 * scale, cy) == gdip::Ok
        && gdip::GdipDrawPath(graphics.0, pen.0, path.0) == gdip::Ok
        && gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

unsafe fn icon_path_antialiased(
    hdc: HDC,
    rect: RECT,
    dpi: u32,
    color: COLORREF,
    build: impl FnOnce(*mut gdip::GpPath, f32, f32, f32) -> bool,
) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let mut corners = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    if LPtoDP(hdc, corners.as_mut_ptr(), corners.len() as i32) == 0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(hdc) else {
        return false;
    };
    if SetWindowOrgEx(hdc, 0, 0, null_mut()) == 0 || SetViewportOrgEx(hdc, 0, 0, null_mut()) == 0 {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(hdc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let cx = (corners[0].x as f32 + corners[1].x as f32) / 2.0;
    let cy = (corners[0].y as f32 + corners[1].y as f32) / 2.0;
    let scale = 18.0 * dpi.max(96) as f32 / (96.0 * 24.0);
    let Some(pen) = GdiPlusPen::new(color, 2.0 * scale) else {
        return false;
    };
    gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) == gdip::Ok
        && gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) == gdip::Ok
        && gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) == gdip::Ok
        && gdip::GdipSetPenStartCap(pen.0, gdip::LineCapRound) == gdip::Ok
        && gdip::GdipSetPenEndCap(pen.0, gdip::LineCapRound) == gdip::Ok
        && gdip::GdipSetPenLineJoin(pen.0, gdip::LineJoinRound) == gdip::Ok
        && build(path.0, cx, cy, scale)
        && gdip::GdipDrawPath(graphics.0, pen.0, path.0) == gdip::Ok
        && gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

fn gear_points(cx: f32, cy: f32, scale: f32) -> [gdip::PointF; 36] {
    std::array::from_fn(|index| {
        let tooth = index / 6;
        let step = index % 6;
        let angle = (tooth as f32 * 60.0 - 90.0
            + [-30.0_f32, -17.0, -12.0, 12.0, 17.0, 30.0][step])
            .to_radians();
        let radius = if step == 2 || step == 3 { 10.5 } else { 7.8 };
        let (sin, cos) = angle.sin_cos();
        gdip::PointF {
            X: cx + cos * radius * scale,
            Y: cy + sin * radius * scale,
        }
    })
}

unsafe fn settings_icon(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    if icon_path_antialiased(hdc, rect, dpi, color, |path, cx, cy, scale| {
        let points = gear_points(cx, cy, scale);
        gdip::GdipAddPathLine2(path, points.as_ptr(), points.len() as i32) == gdip::Ok
            && gdip::GdipClosePathFigure(path) == gdip::Ok
            && gdip::GdipStartPathFigure(path) == gdip::Ok
            && gdip::GdipAddPathEllipse(
                path,
                cx - 3.2 * scale,
                cy - 3.2 * scale,
                6.4 * scale,
                6.4 * scale,
            ) == gdip::Ok
    }) {
        return;
    }
    let cx = (rect.left as f32 + rect.right as f32) / 2.0;
    let cy = (rect.top as f32 + rect.bottom as f32) / 2.0;
    let scale = 18.0 * dpi.max(96) as f32 / (96.0 * 24.0);
    let points = gear_points(cx, cy, scale).map(|point| POINT {
        x: point.X.round() as i32,
        y: point.Y.round() as i32,
    });
    let brush = LOGBRUSH {
        lbStyle: BS_SOLID,
        lbColor: color,
        lbHatch: 0,
    };
    let stroke = ((dpi.max(96) as u64 * 3 + 96) / 192) as u32;
    let pen = ExtCreatePen(
        (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32,
        stroke,
        &brush,
        0,
        null(),
    );
    if pen.is_null() {
        return;
    }
    let previous_pen = SelectObject(hdc, pen);
    let previous_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    Polygon(hdc, points.as_ptr(), points.len() as i32);
    let hole = 3.2 * scale;
    Ellipse(
        hdc,
        (cx - hole).round() as i32,
        (cy - hole).round() as i32,
        (cx + hole).round() as i32,
        (cy + hole).round() as i32,
    );
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    DeleteObject(pen);
}

// 图形改编自 Lucide focus，许可见 assets/lucide-focus-LICENSE.txt
unsafe fn picker_icon(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    if !picker_icon_antialiased(hdc, rect, dpi, color) {
        picker_icon_gdi(hdc, rect, dpi, color);
    }
}

unsafe fn picker_icon_antialiased(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) -> bool {
    icon_path_antialiased(hdc, rect, dpi, color, |path, cx, cy, scale| {
        let point = |x: f32, y: f32, quarter: usize| {
            let (x, y) = match quarter {
                1 => (24.0 - y, x),
                2 => (24.0 - x, 24.0 - y),
                3 => (y, 24.0 - x),
                _ => (x, y),
            };
            gdip::PointF {
                X: cx + (x - 12.0) * scale,
                Y: cy + (y - 12.0) * scale,
            }
        };
        for quarter in 0..4 {
            let start = point(3.0, 7.0, quarter);
            let corner = point(3.0, 5.0, quarter);
            let control1 = point(3.0, 5.0 - 2.0 * 0.552_284_8, quarter);
            let control2 = point(5.0 - 2.0 * 0.552_284_8, 3.0, quarter);
            let curve_end = point(5.0, 3.0, quarter);
            let end = point(7.0, 3.0, quarter);
            if gdip::GdipStartPathFigure(path) != gdip::Ok
                || gdip::GdipAddPathLine(path, start.X, start.Y, corner.X, corner.Y) != gdip::Ok
                || gdip::GdipAddPathBezier(
                    path,
                    corner.X,
                    corner.Y,
                    control1.X,
                    control1.Y,
                    control2.X,
                    control2.Y,
                    curve_end.X,
                    curve_end.Y,
                ) != gdip::Ok
                || gdip::GdipAddPathLine(path, curve_end.X, curve_end.Y, end.X, end.Y) != gdip::Ok
            {
                return false;
            }
        }
        gdip::GdipStartPathFigure(path) == gdip::Ok
            && gdip::GdipAddPathEllipse(
                path,
                cx - 3.0 * scale,
                cy - 3.0 * scale,
                6.0 * scale,
                6.0 * scale,
            ) == gdip::Ok
    })
}

unsafe fn picker_icon_gdi(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    let cx = (rect.left as f64 + rect.right as f64) / 2.0;
    let cy = (rect.top as f64 + rect.bottom as f64) / 2.0;
    let scale = 18.0 * dpi.max(96) as f64 / (96.0 * 24.0);
    let point = |x: f64, y: f64, quarter: usize| {
        let (x, y) = match quarter {
            1 => (24.0 - y, x),
            2 => (24.0 - x, 24.0 - y),
            3 => (y, 24.0 - x),
            _ => (x, y),
        };
        POINT {
            x: (cx + (x - 12.0) * scale).round() as i32,
            y: (cy + (y - 12.0) * scale).round() as i32,
        }
    };
    let brush = LOGBRUSH {
        lbStyle: BS_SOLID,
        lbColor: color,
        lbHatch: 0,
    };
    let stroke = ((dpi.max(96) as u64 * 3 + 96) / 192) as u32;
    let pen = ExtCreatePen(
        (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32,
        stroke,
        &brush,
        0,
        null(),
    );
    if pen.is_null() {
        return;
    }
    let previous_pen = SelectObject(hdc, pen);
    let previous_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    if BeginPath(hdc) != 0 {
        let tangent = 0.552_284_749_830_793_6;
        let mut complete = true;
        for quarter in 0..4 {
            let start = point(3.0, 7.0, quarter);
            let corner = point(3.0, 5.0, quarter);
            let end = point(7.0, 3.0, quarter);
            let curve = [
                point(3.0, 5.0 - 2.0 * tangent, quarter),
                point(5.0 - 2.0 * tangent, 3.0, quarter),
                point(5.0, 3.0, quarter),
            ];
            complete &= MoveToEx(hdc, start.x, start.y, null_mut()) != 0;
            complete &= LineTo(hdc, corner.x, corner.y) != 0;
            complete &= PolyBezierTo(hdc, curve.as_ptr(), curve.len() as u32) != 0;
            complete &= LineTo(hdc, end.x, end.y) != 0;
        }
        let start = point(15.0, 12.0, 0);
        complete &= MoveToEx(hdc, start.x, start.y, null_mut()) != 0;
        for quarter in 0..4 {
            let curve = [
                point(15.0, 12.0 + 3.0 * tangent, quarter),
                point(12.0 + 3.0 * tangent, 15.0, quarter),
                point(12.0, 15.0, quarter),
            ];
            complete &= PolyBezierTo(hdc, curve.as_ptr(), curve.len() as u32) != 0;
        }
        complete &= CloseFigure(hdc) != 0;
        if complete && EndPath(hdc) != 0 {
            if StrokePath(hdc) == 0 {
                AbortPath(hdc);
            }
        } else {
            AbortPath(hdc);
        }
    }
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    DeleteObject(pen);
}

// 图形改编自 Lucide rotate-cw，原始路径和许可见 assets/lucide-rotate-cw.svg 与对应 LICENSE
unsafe fn refresh_icon(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    if !refresh_icon_antialiased(hdc, rect, dpi, color) {
        refresh_icon_gdi(hdc, rect, dpi, color);
    }
}

unsafe fn refresh_icon_antialiased(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) -> bool {
    if !GDIPLUS_READY.with(Cell::get) {
        return false;
    }
    let mut corners = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    if LPtoDP(hdc, corners.as_mut_ptr(), corners.len() as i32) == 0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(hdc) else {
        return false;
    };
    // 先转为画布像素坐标，再清零原点，兼容缓冲区的非零窗口原点
    if SetWindowOrgEx(hdc, 0, 0, null_mut()) == 0 || SetViewportOrgEx(hdc, 0, 0, null_mut()) == 0 {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(hdc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let cx = (corners[0].x as f32 + corners[1].x as f32) / 2.0;
    let cy = (corners[0].y as f32 + corners[1].y as f32) / 2.0;
    let scale = 18.0 * dpi.max(96) as f32 / (96.0 * 24.0);
    let point = |x: f32, y: f32| gdip::PointF {
        X: cx + (x - 12.0) * scale,
        Y: cy + (y - 12.0) * scale,
    };
    let Some(pen) = GdiPlusPen::new(color, 2.0 * scale) else {
        return false;
    };
    // 保留路径和描边的亚像素精度，避免小尺寸圆弧先取整后出现锯齿
    let tangent = 9.0 * 0.552_284_8;
    let curve = [
        point(21.0, 12.0),
        point(21.0, 12.0 + tangent),
        point(12.0 + tangent, 21.0),
        point(12.0, 21.0),
        point(12.0 - tangent, 21.0),
        point(3.0, 12.0 + tangent),
        point(3.0, 12.0),
        point(3.0, 12.0 - tangent),
        point(12.0 - tangent, 3.0),
        point(12.0, 3.0),
        point(14.52, 3.0),
        point(16.93, 4.0),
        point(18.74, 5.74),
    ];
    let arrow_start = point(21.0, 3.0);
    let arrow_tip = point(21.0, 8.0);
    let arrow_end = point(16.0, 8.0);
    let curve_end = curve[curve.len() - 1];
    if gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) != gdip::Ok
        || gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) != gdip::Ok
        || gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) != gdip::Ok
        || gdip::GdipSetPenStartCap(pen.0, gdip::LineCapRound) != gdip::Ok
        || gdip::GdipSetPenEndCap(pen.0, gdip::LineCapRound) != gdip::Ok
        || gdip::GdipSetPenLineJoin(pen.0, gdip::LineJoinRound) != gdip::Ok
        || gdip::GdipAddPathBeziers(path.0, curve.as_ptr(), curve.len() as i32) != gdip::Ok
        || gdip::GdipAddPathLine(path.0, curve_end.X, curve_end.Y, arrow_tip.X, arrow_tip.Y)
            != gdip::Ok
        || gdip::GdipStartPathFigure(path.0) != gdip::Ok
        || gdip::GdipAddPathLine(
            path.0,
            arrow_start.X,
            arrow_start.Y,
            arrow_tip.X,
            arrow_tip.Y,
        ) != gdip::Ok
        || gdip::GdipAddPathLine(path.0, arrow_tip.X, arrow_tip.Y, arrow_end.X, arrow_end.Y)
            != gdip::Ok
        || gdip::GdipDrawPath(graphics.0, pen.0, path.0) != gdip::Ok
    {
        return false;
    }
    // 完成 GDI+ 绘制后再让调用方把缓冲区复制到窗口
    gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

unsafe fn refresh_icon_gdi(hdc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    let cx = (rect.left as f64 + rect.right as f64) / 2.0;
    let cy = (rect.top as f64 + rect.bottom as f64) / 2.0;
    let scale = 18.0 * dpi.max(96) as f64 / (96.0 * 24.0);
    let point = |x: f64, y: f64| POINT {
        x: (cx + (x - 12.0) * scale).round() as i32,
        y: (cy + (y - 12.0) * scale).round() as i32,
    };
    let brush = LOGBRUSH {
        lbStyle: BS_SOLID,
        lbColor: color,
        lbHatch: 0,
    };
    let stroke = ((dpi.max(96) as u64 * 3 + 96) / 192) as u32;
    let pen = ExtCreatePen(
        (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32,
        stroke,
        &brush,
        0,
        null(),
    );
    if pen.is_null() {
        return;
    }
    let previous_pen = SelectObject(hdc, pen);
    let previous_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    if BeginPath(hdc) != 0 {
        let mut complete = true;
        let start = point(21.0, 12.0);
        let tangent = 9.0 * 0.552_284_749_830_793_6;
        let curve = [
            point(21.0, 12.0 + tangent),
            point(12.0 + tangent, 21.0),
            point(12.0, 21.0),
            point(12.0 - tangent, 21.0),
            point(3.0, 12.0 + tangent),
            point(3.0, 12.0),
            point(3.0, 12.0 - tangent),
            point(12.0 - tangent, 3.0),
            point(12.0, 3.0),
            point(14.52, 3.0),
            point(16.93, 4.0),
            point(18.74, 5.74),
        ];
        complete &= MoveToEx(hdc, start.x, start.y, null_mut()) != 0;
        complete &= PolyBezierTo(hdc, curve.as_ptr(), curve.len() as u32) != 0;
        let arrow_start = point(21.0, 3.0);
        let arrow_tip = point(21.0, 8.0);
        let arrow_end = point(16.0, 8.0);
        complete &= LineTo(hdc, arrow_tip.x, arrow_tip.y) != 0;
        complete &= MoveToEx(hdc, arrow_start.x, arrow_start.y, null_mut()) != 0;
        complete &= LineTo(hdc, arrow_tip.x, arrow_tip.y) != 0;
        complete &= LineTo(hdc, arrow_end.x, arrow_end.y) != 0;
        if complete && EndPath(hdc) != 0 {
            if StrokePath(hdc) == 0 {
                AbortPath(hdc);
            }
        } else {
            AbortPath(hdc);
        }
    }
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    DeleteObject(pen);
}

pub unsafe fn draw_button(item: &DRAWITEMSTRUCT, style: ButtonStyle, selected: bool) {
    if let Some(buffer) = PaintBuffer::new(item.hDC, item.rcItem) {
        let buffered_item = DRAWITEMSTRUCT {
            hDC: buffer.dc(),
            ..*item
        };
        if draw_button_content(&buffered_item, style, selected) && buffer.present(item.hDC) {
            return;
        }
    }
    draw_button_content(item, style, selected);
}

unsafe fn draw_button_content(item: &DRAWITEMSTRUCT, style: ButtonStyle, selected: bool) -> bool {
    let Some(_saved) = SavedDc::new(item.hDC) else {
        return false;
    };
    let dpi = GetDpiForWindow(item.hwndItem).max(96);
    let disabled = item.itemState & (ODS_DISABLED | ODS_GRAYED) != 0;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let hot = !disabled && hovered(item.hwndItem);
    let picker = matches!(style, ButtonStyle::Picker);
    let (background, border, foreground) = if disabled {
        (SUBTLE, BORDER, DISABLED_TEXT)
    } else if matches!(style, ButtonStyle::Primary) {
        let background = if pressed {
            ACCENT_PRESSED
        } else if hot {
            ACCENT_HOVER
        } else {
            ACCENT
        };
        (background, background, SURFACE)
    } else if picker {
        if pressed {
            (ACCENT, ACCENT_PRESSED, SURFACE)
        } else if hot {
            (PICKER_HOVER, ACCENT, ACCENT_HOVER)
        } else {
            (ACCENT_SOFT, PICKER_BORDER, ACCENT)
        }
    } else if selected {
        (if pressed { PRESSED } else { ACCENT_SOFT }, ACCENT, ACCENT)
    } else if pressed {
        (PRESSED, BORDER, TEXT)
    } else if hot {
        (HOVER, BORDER, TEXT)
    } else {
        (
            SURFACE,
            BORDER,
            if matches!(style, ButtonStyle::Icon | ButtonStyle::Back) {
                MUTED
            } else {
                TEXT
            },
        )
    };
    fill(item.hDC, &item.rcItem, BACKGROUND);
    let outline = inset(item.rcItem, pixels(1, dpi));
    rounded(item.hDC, &outline, pixels(7, dpi), border);
    let face = inset(outline, pixels(1, dpi));
    rounded(item.hDC, &face, pixels(6, dpi), background);
    let mut content = inset(face, pixels(4, dpi));
    content.top = face.top + pixels(1, dpi);
    content.bottom = face.bottom - pixels(1, dpi);
    if picker {
        picker_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Refresh) {
        refresh_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Back) {
        back_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Icon) {
        settings_icon(item.hDC, item.rcItem, dpi, foreground);
    } else {
        let length = GetWindowTextLengthW(item.hwndItem).max(0) as usize;
        let mut text = vec![0_u16; length + 1];
        let copied = GetWindowTextW(item.hwndItem, text.as_mut_ptr(), text.len() as i32);
        font_for(item.hDC, item.hwndItem);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        SetTextColor(item.hDC, foreground);
        DrawTextW(
            item.hDC,
            text.as_ptr(),
            copied,
            &mut content,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
    if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
        let focus = inset(outline, pixels(4, dpi));
        DrawFocusRect(item.hDC, &focus);
    }
    true
}

pub unsafe fn draw_trackbar(draw: &NMCUSTOMDRAW, compact: bool) -> LRESULT {
    if draw.dwDrawStage == CDDS_PREPAINT {
        return CDRF_NOTIFYITEMDRAW as LRESULT;
    }
    if draw.dwDrawStage != CDDS_ITEMPREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let Some(_saved) = SavedDc::new(draw.hdc) else {
        return CDRF_DODEFAULT as LRESULT;
    };
    let hwnd = draw.hdr.hwndFrom;
    let dpi = GetDpiForWindow(hwnd).max(96);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let mut thumb: RECT = zeroed();
    SendMessageW(
        hwnd,
        TBM_GETTHUMBRECT,
        0,
        (&mut thumb as *mut RECT) as LPARAM,
    );
    let cx = (thumb.left + thumb.right) / 2;
    let cy = (thumb.top + thumb.bottom) / 2;
    match draw.dwItemSpec as u32 {
        TBCD_CHANNEL => {
            let mut channel: RECT = zeroed();
            SendMessageW(
                hwnd,
                TBM_GETCHANNELRECT,
                0,
                (&mut channel as *mut RECT) as LPARAM,
            );
            let height = pixels(if compact { 3 } else { 4 }, dpi);
            channel.top = cy - height / 2;
            channel.bottom = channel.top + height;
            rounded(draw.hdc, &channel, height, TRACK);
            if enabled && cx > channel.left {
                channel.right = cx.min(channel.right);
                rounded(draw.hdc, &channel, height, ACCENT);
            }
            CDRF_SKIPDEFAULT as LRESULT
        }
        TBCD_THUMB => {
            let radius = pixels(if compact { 5 } else { 8 }, dpi);
            // 主手柄不超出原生命中矩形，避免可见圆边缘无法开始拖动
            let radius = if compact {
                radius
            } else {
                radius.min(((thumb.right - thumb.left).min(thumb.bottom - thumb.top) / 2).max(1))
            };
            let circle = RECT {
                left: cx - radius,
                top: cy - radius,
                right: cx + radius,
                bottom: cy + radius,
            };
            let keyboard_focus = GetFocus() == hwnd
                && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) as u32 & UISF_HIDEFOCUS == 0;
            if enabled && (draw.uItemState & (CDIS_HOT | CDIS_SELECTED) != 0 || keyboard_focus) {
                let halo = inset(circle, -pixels(2, dpi));
                rounded(draw.hdc, &halo, radius + pixels(2, dpi), ACCENT_SOFT);
            }
            rounded(
                draw.hdc,
                &circle,
                radius,
                if enabled { ACCENT } else { DISABLED_TEXT },
            );
            let center = inset(circle, pixels(2, dpi));
            rounded(draw.hdc, &center, radius, SURFACE);
            CDRF_SKIPDEFAULT as LRESULT
        }
        TBCD_TICS => CDRF_SKIPDEFAULT as LRESULT,
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

unsafe fn draw_list_name(draw: &NMLVCUSTOMDRAW, hidden: bool) -> bool {
    let list = draw.nmcd.hdr.hwndFrom;
    let row = draw.nmcd.dwItemSpec;
    let dc = draw.nmcd.hdc;
    let images = SendMessageW(list, LVM_GETIMAGELIST, LVSIL_SMALL as usize, 0) as HIMAGELIST;
    let mut bounds = RECT {
        left: LVIR_BOUNDS as i32,
        ..zeroed()
    };
    let mut label = RECT {
        left: LVIR_LABEL as i32,
        ..zeroed()
    };
    let column_width = SendMessageW(list, LVM_GETCOLUMNWIDTH, 0, 0) as i32;
    if column_width <= 0
        || SendMessageW(
            list,
            LVM_GETITEMRECT,
            row,
            &mut bounds as *mut RECT as LPARAM,
        ) == 0
        || (images != 0
            && SendMessageW(
                list,
                LVM_GETITEMRECT,
                row,
                &mut label as *mut RECT as LPARAM,
            ) == 0)
    {
        return false;
    }
    // DrawTextExW 的省略处理最多额外写入四个字符
    let mut text = vec![0_u16; 1028];
    let mut item = LVITEMW {
        mask: LVIF_IMAGE | LVIF_STATE,
        iItem: row as i32,
        stateMask: LVIS_SELECTED | LVIS_CUT,
        ..zeroed()
    };
    if SendMessageW(list, LVM_GETITEMW, 0, &mut item as *mut LVITEMW as LPARAM) == 0 {
        return false;
    }
    let mut name = LVITEMW {
        pszText: text.as_mut_ptr(),
        cchTextMax: 1024,
        ..zeroed()
    };
    let length = SendMessageW(
        list,
        LVM_GETITEMTEXTW,
        row,
        &mut name as *mut LVITEMW as LPARAM,
    );
    if length <= 0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let cell = RECT {
        right: bounds.left + column_width,
        ..bounds
    };
    if IntersectClipRect(dc, cell.left, cell.top, cell.right, cell.bottom) == ERROR {
        return false;
    }
    let selected = item.state & LVIS_SELECTED != 0;
    let focused = GetFocus() == list;
    let mut foreground = if selected {
        GetSysColor(if focused {
            COLOR_HIGHLIGHTTEXT
        } else {
            COLOR_BTNTEXT
        })
    } else {
        TEXT
    };
    let background = if selected {
        GetSysColor(if focused {
            COLOR_HIGHLIGHT
        } else {
            COLOR_3DFACE
        })
    } else {
        SURFACE
    };
    fill(dc, &cell, background);
    if selected {
        let theme = GetWindowTheme(list);
        if theme != 0 {
            let state = if focused {
                LISS_SELECTED
            } else {
                LISS_SELECTEDNOTFOCUS
            };
            if DrawThemeBackground(theme, dc, LVP_LISTITEM, state, &bounds, &cell) >= 0 {
                let mut themed_text = foreground;
                if GetThemeColor(
                    theme,
                    LVP_LISTITEM,
                    state,
                    TMT_TEXTCOLOR as i32,
                    &mut themed_text,
                ) >= 0
                {
                    foreground = themed_text;
                }
            }
        }
    }
    if images != 0 && item.iImage >= 0 {
        let mut icon = RECT {
            left: LVIR_ICON as i32,
            ..zeroed()
        };
        let mut icon_width = 0;
        let mut icon_height = 0;
        if SendMessageW(list, LVM_GETITEMRECT, row, &mut icon as *mut RECT as LPARAM) != 0
            && ImageList_GetIconSize(images, &mut icon_width, &mut icon_height) != 0
        {
            ImageList_Draw(
                images,
                item.iImage,
                dc,
                icon.left,
                cell.top + (cell.bottom - cell.top - icon_height) / 2,
                ILD_TRANSPARENT
                    | if item.state & LVIS_CUT != 0 {
                        ILD_BLEND50
                    } else {
                        0
                    },
            );
        }
    }
    let dpi = GetDpiForWindow(list).max(96);
    let badge_size = if hidden { pixels(7, dpi) } else { 0 };
    let gap = if hidden { pixels(2, dpi) } else { 0 };
    // 解除图片列表后不沿用原生标签的图标占位宽度
    let text_left = cell.left + pixels(4, dpi);
    let mut content = RECT {
        left: if images != 0 {
            label.left.max(text_left)
        } else {
            text_left
        },
        right: cell.right - pixels(4, dpi) - badge_size - gap,
        ..cell
    };
    if content.right <= content.left {
        return false;
    }
    font_for(dc, list);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, foreground);
    if DrawTextExW(
        dc,
        text.as_mut_ptr(),
        length as i32,
        &mut content,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_MODIFYSTRING | DT_NOPREFIX,
        null(),
    ) == 0
    {
        return false;
    }
    if !hidden {
        return true;
    }
    let shown_length = text.iter().position(|value| *value == 0).unwrap_or(1024);
    let mut extent: SIZE = zeroed();
    if GetTextExtentPoint32W(dc, text.as_ptr(), shown_length as i32, &mut extent) == 0 {
        return false;
    }
    // 角标紧贴实际显示名称的末端，并略高于文字基线
    let left = (content.left + extent.cx + gap).min(cell.right - pixels(4, dpi) - badge_size);
    let top = ((cell.top + cell.bottom) / 2 - pixels(9, dpi)).max(cell.top + pixels(1, dpi));
    let pen = CreatePen(PS_SOLID, pixels(1, dpi).max(1), foreground);
    if !pen.is_null() {
        SelectObject(dc, pen);
    } else {
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCPenColor(dc, foreground);
    }
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    Ellipse(dc, left, top, left + badge_size, top + badge_size);
    MoveToEx(
        dc,
        left + pixels(2, dpi),
        top + badge_size - pixels(2, dpi),
        null_mut(),
    );
    LineTo(dc, left + badge_size - pixels(2, dpi), top + pixels(2, dpi));
    // 先恢复 DC 再释放选入的画笔
    drop(_saved);
    if !pen.is_null() {
        DeleteObject(pen);
    }
    true
}

pub unsafe fn draw_list(draw: &mut NMLVCUSTOMDRAW) -> LRESULT {
    let hidden = hidden_window(draw.nmcd.lItemlParam as usize);
    let custom_name = hidden
        || SendMessageW(
            draw.nmcd.hdr.hwndFrom,
            LVM_GETIMAGELIST,
            LVSIL_SMALL as usize,
            0,
        ) == 0;
    match draw.nmcd.dwDrawStage {
        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW as LRESULT,
        CDDS_ITEMPREPAINT => {
            let state = SendMessageW(
                draw.nmcd.hdr.hwndFrom,
                LVM_GETITEMSTATE,
                draw.nmcd.dwItemSpec,
                LVIS_SELECTED as LPARAM,
            ) as u32;
            let notify_subitems = if custom_name {
                CDRF_NOTIFYSUBITEMDRAW
            } else {
                0
            };
            if state & LVIS_SELECTED != 0 {
                return notify_subitems as LRESULT;
            }
            draw.clrText = TEXT;
            draw.clrTextBk = SURFACE;
            (CDRF_NEWFONT | notify_subitems) as LRESULT
        }
        stage if stage == CDDS_ITEMPREPAINT | CDDS_SUBITEM && custom_name && draw.iSubItem == 0 => {
            if draw_list_name(draw, hidden) {
                CDRF_SKIPDEFAULT as LRESULT
            } else {
                CDRF_DODEFAULT as LRESULT
            }
        }
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

pub unsafe fn draw_header(draw: &NMCUSTOMDRAW) -> LRESULT {
    if draw.dwDrawStage == CDDS_PREPAINT {
        return CDRF_NOTIFYITEMDRAW as LRESULT;
    }
    if draw.dwDrawStage != CDDS_ITEMPREPAINT {
        return CDRF_DODEFAULT as LRESULT;
    }
    let Some(_saved) = SavedDc::new(draw.hdc) else {
        return CDRF_DODEFAULT as LRESULT;
    };
    let hwnd = draw.hdr.hwndFrom;
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut text = [0_u16; 256];
    let mut item = HDITEMW {
        mask: HDI_TEXT | HDI_FORMAT,
        pszText: text.as_mut_ptr(),
        cchTextMax: text.len() as i32,
        ..zeroed()
    };
    if SendMessageW(
        hwnd,
        HDM_GETITEMW,
        draw.dwItemSpec,
        (&mut item as *mut HDITEMW) as LPARAM,
    ) == 0
    {
        return CDRF_DODEFAULT as LRESULT;
    }
    fill(draw.hdc, &draw.rc, SUBTLE);
    let separator = RECT {
        top: draw.rc.bottom - pixels(1, dpi),
        ..draw.rc
    };
    fill(draw.hdc, &separator, BORDER);
    let mut content = draw.rc;
    content.left += pixels(8, dpi);
    content.right -= pixels(8, dpi);
    font_for(draw.hdc, hwnd);
    SetBkMode(draw.hdc, TRANSPARENT as i32);
    SetTextColor(draw.hdc, MUTED);
    let align = if item.fmt & HDF_RIGHT != 0 {
        DT_RIGHT
    } else if item.fmt & HDF_CENTER != 0 {
        DT_CENTER
    } else {
        DT_LEFT
    };
    DrawTextW(
        draw.hdc,
        text.as_ptr(),
        -1,
        &mut content,
        align | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    CDRF_SKIPDEFAULT as LRESULT
}
