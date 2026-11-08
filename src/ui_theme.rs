use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus as gdip;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetFocus, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
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
const TRACKBAR_THEME_SUBCLASS: usize = 0x5452_5352;

thread_local! {
    static WINDOW_BADGES: RefCell<HashMap<usize, WindowBadges>> = RefCell::new(HashMap::new());
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

#[derive(Clone, Copy, Default)]
pub struct WindowBadges {
    pub hidden: bool,
    pub topmost: bool,
    pub container: bool,
}

impl WindowBadges {
    fn any(self) -> bool {
        self.hidden || self.topmost || self.container
    }
}

pub fn set_window_badges(windows: impl IntoIterator<Item = (usize, WindowBadges)>) {
    let windows = windows
        .into_iter()
        .filter(|(_, badges)| badges.any())
        .collect();
    WINDOW_BADGES.with(|badges| *badges.borrow_mut() = windows);
}

fn window_badges(window: usize) -> WindowBadges {
    WINDOW_BADGES.with(|badges| badges.borrow().get(&window).copied().unwrap_or_default())
}

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Picker,
    Refresh,
    NameVisibility,
    MiniMode,
    Back,
    OpenWindow,
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
        (level != 0).then(|| Self { hdc, level })
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
    draw_rounded_surface(hdc, rect, radius, color, None);
}

// 矩形使用客户区逻辑坐标，半径和边框宽度使用物理像素
pub unsafe fn draw_rounded_surface(
    dc: HDC,
    rect: &RECT,
    radius: i32,
    color: COLORREF,
    border: Option<(COLORREF, i32)>,
) -> bool {
    if dc.is_null() || rect.right <= rect.left || rect.bottom <= rect.top {
        return false;
    }
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let short_side = (rect.right - rect.left).min(rect.bottom - rect.top);
    let radius = radius.max(0).min((short_side + 1) / 2);
    let border = border
        .filter(|(_, width)| *width > 0)
        .map(|(color, width)| (color, width.min(short_side)));
    if rounded_surface_antialiased(dc, rect, radius, color, border) {
        return true;
    }
    // GDI+ 不可用时保留原生绘制，不重复尝试已经失败的抗锯齿路径
    if let Some((border_color, width)) = border {
        if !rounded_gdi(dc, rect, radius, border_color) {
            return false;
        }
        let inner = inset(*rect, width);
        if inner.right > inner.left && inner.bottom > inner.top {
            return rounded_gdi(dc, &inner, (radius - width).max(0), color);
        }
        true
    } else {
        rounded_gdi(dc, rect, radius, color)
    }
}

unsafe fn rounded_surface_antialiased(
    dc: HDC,
    rect: &RECT,
    radius: i32,
    color: COLORREF,
    border: Option<(COLORREF, i32)>,
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
    if LPtoDP(dc, corners.as_mut_ptr(), corners.len() as i32) == 0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    if SetWindowOrgEx(dc, 0, 0, null_mut()) == 0 || SetViewportOrgEx(dc, 0, 0, null_mut()) == 0 {
        return false;
    }
    let Some(graphics) = GdiPlusGraphics::new(dc) else {
        return false;
    };
    let Some(path) = GdiPlusPath::new() else {
        return false;
    };
    let short_side = (corners[1].x - corners[0].x).min(corners[1].y - corners[0].y) as f32;
    if short_side <= 0.0 {
        return false;
    }
    let (color, border) = match border {
        Some((border_color, width)) if width as f32 >= short_side => (border_color, None),
        _ => (color, border),
    };
    let stroke = border.map_or(0.0, |(_, width)| width as f32);
    let half_stroke = stroke / 2.0;
    // 路径按半笔宽内缩，让笔画外边缘落在整数像素边界
    let left = corners[0].x as f32 + half_stroke;
    let top = corners[0].y as f32 + half_stroke;
    let right = corners[1].x as f32 - half_stroke;
    let bottom = corners[1].y as f32 - half_stroke;
    let diameter = (2.0 * radius as f32 - stroke)
        .max(0.0)
        .min(right - left)
        .min(bottom - top);
    let Some(brush) = GdiPlusBrush::new(color) else {
        return false;
    };
    let pen = match border {
        Some((color, _)) => match GdiPlusPen::new(color, stroke) {
            Some(pen) => Some(pen),
            None => return false,
        },
        None => None,
    };
    if gdip::GdipSetPageUnit(graphics.0, gdip::UnitPixel) != gdip::Ok
        || gdip::GdipSetSmoothingMode(graphics.0, gdip::SmoothingModeAntiAlias8x8) != gdip::Ok
        || gdip::GdipSetPixelOffsetMode(graphics.0, gdip::PixelOffsetModeHighQuality) != gdip::Ok
    {
        return false;
    }
    if !rounded_path(&path, left, top, right, bottom, diameter)
        || gdip::GdipFillPath(graphics.0, brush.0.cast(), path.0) != gdip::Ok
    {
        return false;
    }
    if let Some(pen) = pen {
        if gdip::GdipDrawPath(graphics.0, pen.0, path.0) != gdip::Ok {
            return false;
        }
    }
    gdip::GdipFlush(graphics.0, gdip::FlushIntentionSync) == gdip::Ok
}

unsafe fn rounded_gdi(hdc: HDC, rect: &RECT, radius: i32, color: COLORREF) -> bool {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return false;
    }
    SetDCBrushColor(hdc, color);
    SelectObject(hdc, GetStockObject(DC_BRUSH));
    SelectObject(hdc, GetStockObject(NULL_PEN));
    if radius <= 0 {
        return FillRect(hdc, rect, GetStockObject(DC_BRUSH)) != 0;
    }
    RoundRect(
        hdc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        radius * 2,
        radius * 2,
    ) != 0
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

pub unsafe fn install_checkbox_theme(hwnd: HWND) -> bool {
    if SetWindowSubclass(
        hwnd,
        Some(compact_checkbox_proc),
        COMPACT_CHECKBOX_SUBCLASS,
        0,
    ) == 0
    {
        return false;
    }
    SendMessageW(
        hwnd,
        WM_UPDATEUISTATE,
        (UIS_SET | (UISF_HIDEFOCUS << 16)) as usize,
        0,
    );
    true
}

unsafe fn checkbox_parent_ui_state(hwnd: HWND) -> u32 {
    SendMessageW(GetParent(hwnd), WM_QUERYUISTATE, 0, 0) as u32
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
        WM_UPDATEUISTATE => {
            // 只隐藏本控件的原生焦点框，自绘焦点仍遵循父窗口的键盘提示状态
            let desired = checkbox_parent_ui_state(hwnd) | UISF_HIDEFOCUS;
            let flags = UISF_HIDEFOCUS | UISF_HIDEACCEL | UISF_ACTIVE;
            for (action, mask) in [(UIS_SET, desired & flags), (UIS_CLEAR, !desired & flags)] {
                if mask != 0 {
                    DefSubclassProc(hwnd, message, (action | (mask << 16)) as usize, 0);
                }
            }
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
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

pub unsafe fn compact_checkbox_width(hwnd: HWND) -> Option<i32> {
    let dc = GetDC(hwnd);
    if dc.is_null() {
        return None;
    }
    let width = (|| {
        let _saved = SavedDc::new(dc)?;
        font_for(dc, hwnd);
        let length = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut text = vec![0_u16; length + 1];
        let copied = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
        let mut size: SIZE = zeroed();
        if copied <= 0 || GetTextExtentPoint32W(dc, text.as_ptr(), copied, &mut size) == 0 {
            return None;
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        Some(size.cx + pixels(12, dpi) + pixels(4, dpi))
    })();
    ReleaseDC(hwnd, dc);
    width
}

unsafe fn draw_compact_checkbox(hwnd: HWND, dc: HDC, rect: RECT, hovered: bool) -> bool {
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) != BST_UNCHECKED as isize;
    let pressed = SendMessageW(hwnd, BM_GETSTATE, 0, 0) as u32 & BST_PUSHED != 0;
    let focused =
        enabled && GetFocus() == hwnd && checkbox_parent_ui_state(hwnd) & UISF_HIDEFOCUS == 0;
    let right_aligned = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & BS_RIGHT as u32 != 0;
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0_u16; length + 1];
    let copied = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
    font_for(dc, hwnd);
    let mut size: SIZE = zeroed();
    GetTextExtentPoint32W(dc, text.as_ptr(), copied, &mut size);
    let side = pixels(if right_aligned { 12 } else { 13 }, dpi);
    let gap = pixels(4, dpi);
    let text_left = if right_aligned {
        (rect.right - size.cx).max(rect.left + side + gap)
    } else {
        rect.left + side + gap
    };
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
    } else if checked || pressed || focused {
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
    } else if focused {
        if checked {
            PICKER_HOVER
        } else {
            ACCENT_SOFT
        }
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
        rounded_gdi(dc, &glyph, pixels(3, dpi), border);
        rounded_gdi(dc, &inset(glyph, pixels(1, dpi)), pixels(2, dpi), face);
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
    SetTextColor(
        dc,
        if !enabled {
            DISABLED_TEXT
        } else if focused {
            ACCENT
        } else {
            TEXT
        },
    );
    let alignment = if right_aligned { DT_RIGHT } else { DT_LEFT };
    DrawTextW(
        dc,
        text.as_ptr(),
        copied,
        &mut label,
        alignment | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
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
            .zip(*b"EDIT")
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
    // 只重新计算客户区，让原生控件同步维护边缘失效区域
    SetWindowPos(
        hwnd,
        null_mut(),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
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
            return 0;
        }
        WM_GETTEXT if SendMessageW(hwnd, HKM_GETHOTKEY, 0, 0) == 0 => {
            if wparam == 0 || lparam == 0 {
                return 0;
            }
            *(lparam as *mut u16) = 0;
            return 0;
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
    // 空值只绘制背景，组合键内容与输入行为仍由原生控件管理
    fill(dc, &rect, SURFACE);
    font_for(dc, hwnd);
    if update_caret && GetFocus() == hwnd && IsWindowEnabled(hwnd) != 0 {
        let mut metrics: TEXTMETRICW = zeroed();
        if GetTextMetricsW(dc, &mut metrics) != 0 {
            // 只调整原生光标的位置，保留其创建、闪烁与键盘更新机制
            SetCaretPos(rect.left, ((rect.bottom - metrics.tmHeight) / 2).max(0));
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
            rounded_gdi(dc, &outline, pixels(7, dpi), border);
            rounded_gdi(dc, &inset(outline, pixels(1, dpi)), pixels(6, dpi), SURFACE);
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
        SWP_NOZORDER | SWP_NOACTIVATE,
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
        rounded_gdi(dc, &outline, pixels(7, dpi), border);
        rounded_gdi(dc, &inset(outline, pixels(1, dpi)), pixels(6, dpi), SURFACE);
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
    let width = right - left;
    let height = bottom - top;
    if ![left, top, right, bottom, diameter, width, height]
        .into_iter()
        .all(f32::is_finite)
        || width <= 0.0
        || height <= 0.0
    {
        return false;
    }
    let diameter = diameter.max(0.0).min(width).min(height);
    if diameter == 0.0 {
        return gdip::GdipAddPathRectangle(path.0, left, top, width, height) == gdip::Ok;
    }
    gdip::GdipAddPathArc(path.0, left, top, diameter, diameter, 180.0, 90.0) == gdip::Ok
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
    // 半径向上取整后由共用路径限制直径，奇数宽度同样保持完整圆头
    draw_rounded_surface(dc, rect, diameter / 2 + diameter % 2, color, None);
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
    let scale = 18.0 * dpi.max(96) as f32 / (96.0 * 24.0);
    scaled_icon_path_antialiased(hdc, rect, color, scale, 2.0 * scale, build)
}

unsafe fn scaled_icon_path_antialiased(
    hdc: HDC,
    rect: RECT,
    color: COLORREF,
    scale: f32,
    stroke: f32,
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
    let Some(pen) = GdiPlusPen::new(color, stroke) else {
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

pub(crate) unsafe fn draw_caption_button_icon(
    dc: HDC,
    rect: RECT,
    dpi: u32,
    color: COLORREF,
    close: bool,
) -> bool {
    let scale = dpi.max(96) as f32 / 96.0;
    scaled_icon_path_antialiased(dc, rect, color, scale, scale, |path, cx, cy, scale| {
        let half = 4.5 * scale;
        if close {
            gdip::GdipAddPathLine(path, cx - half, cy - half, cx + half, cy + half) == gdip::Ok
                && gdip::GdipStartPathFigure(path) == gdip::Ok
                && gdip::GdipAddPathLine(path, cx + half, cy - half, cx - half, cy + half)
                    == gdip::Ok
        } else {
            gdip::GdipAddPathLine(path, cx - half, cy, cx + half, cy) == gdip::Ok
        }
    })
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

unsafe fn name_visibility_icon(dc: HDC, rect: RECT, dpi: u32, color: COLORREF, visible: bool) {
    let scale = 16.0 * dpi.max(96) as f32 / (96.0 * 24.0);
    if scaled_icon_path_antialiased(
        dc,
        rect,
        color,
        scale,
        2.0 * scale,
        |path, cx, cy, scale| {
            for direction in [-1.0, 1.0] {
                if gdip::GdipStartPathFigure(path) != gdip::Ok
                    || gdip::GdipAddPathBezier(
                        path,
                        cx - 10.0 * scale,
                        cy,
                        cx - 5.0 * scale,
                        cy + direction * 8.0 * scale,
                        cx + 5.0 * scale,
                        cy + direction * 8.0 * scale,
                        cx + 10.0 * scale,
                        cy,
                    ) != gdip::Ok
                {
                    return false;
                }
            }
            if gdip::GdipStartPathFigure(path) != gdip::Ok {
                return false;
            }
            if visible {
                gdip::GdipAddPathEllipse(
                    path,
                    cx - 3.0 * scale,
                    cy - 3.0 * scale,
                    6.0 * scale,
                    6.0 * scale,
                ) == gdip::Ok
            } else {
                gdip::GdipAddPathLine(
                    path,
                    cx - 9.0 * scale,
                    cy - 9.0 * scale,
                    cx + 9.0 * scale,
                    cy + 9.0 * scale,
                ) == gdip::Ok
            }
        },
    ) {
        return;
    }
    let Some(saved) = SavedDc::new(dc) else {
        return;
    };
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let pen = CreatePen(PS_SOLID, (2.0 * scale).round().max(1.0) as i32, color);
    if pen.is_null() {
        return;
    }
    SelectObject(dc, pen);
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    let point = |x: f32, y: f32| POINT {
        x: (cx + x * scale).round() as i32,
        y: (cy + y * scale).round() as i32,
    };
    for direction in [-1.0, 1.0] {
        let curve = [
            point(-10.0, 0.0),
            point(-5.0, direction * 8.0),
            point(5.0, direction * 8.0),
            point(10.0, 0.0),
        ];
        PolyBezier(dc, curve.as_ptr(), curve.len() as u32);
    }
    if visible {
        let first = point(-3.0, -3.0);
        let last = point(3.0, 3.0);
        Ellipse(dc, first.x, first.y, last.x, last.y);
    } else {
        let first = point(-9.0, -9.0);
        let last = point(9.0, 9.0);
        MoveToEx(dc, first.x, first.y, null_mut());
        LineTo(dc, last.x, last.y);
    }
    drop(saved);
    DeleteObject(pen);
}

unsafe fn open_window_icon(dc: HDC, rect: RECT, dpi: u32, color: COLORREF) {
    let lines: [&[(f32, f32)]; 3] = [
        &[
            (-1.5, -5.5),
            (-5.5, -5.5),
            (-5.5, 5.5),
            (5.5, 5.5),
            (5.5, 1.5),
        ],
        &[(-0.5, 0.5), (6.0, -6.0)],
        &[(1.5, -6.0), (6.0, -6.0), (6.0, -1.5)],
    ];
    let scale = dpi.max(96) as f32 / 96.0;
    let stroke = 1.5 * scale;
    if scaled_icon_path_antialiased(dc, rect, color, scale, stroke, |path, cx, cy, scale| {
        for line in lines {
            if gdip::GdipStartPathFigure(path) != gdip::Ok {
                return false;
            }
            for pair in line.windows(2) {
                if gdip::GdipAddPathLine(
                    path,
                    cx + pair[0].0 * scale,
                    cy + pair[0].1 * scale,
                    cx + pair[1].0 * scale,
                    cy + pair[1].1 * scale,
                ) != gdip::Ok
                {
                    return false;
                }
            }
        }
        true
    }) {
        return;
    }
    let Some(saved) = SavedDc::new(dc) else {
        return;
    };
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let pen = CreatePen(PS_SOLID, stroke.round().max(1.0) as i32, color);
    if pen.is_null() {
        return;
    }
    SelectObject(dc, pen);
    for line in lines {
        let mut points: [POINT; 5] = zeroed();
        for (point, &(x, y)) in points.iter_mut().zip(line) {
            point.x = (cx + x * scale).round() as i32;
            point.y = (cy + y * scale).round() as i32;
        }
        Polyline(dc, points.as_ptr(), line.len() as i32);
    }
    drop(saved);
    DeleteObject(pen);
}

unsafe fn mini_mode_icon(dc: HDC, rect: RECT, dpi: u32, color: COLORREF, expand: bool) {
    let tip = if expand { 8.0 } else { 2.0 };
    let tail = if expand { 2.0 } else { 8.0 };
    let wing = if expand { 3.0 } else { 7.0 };
    let dpi_scale = dpi.max(96) as f32 / 96.0;
    let scale = if expand { 11.0 / 16.0 } else { 18.0 / 24.0 } * dpi_scale;
    let stroke = 1.5 * dpi_scale;
    if scaled_icon_path_antialiased(dc, rect, color, scale, stroke, |path, cx, cy, scale| {
        for direction in [-1.0, 1.0] {
            let point = |x: f32, y: f32| gdip::PointF {
                X: cx + x * direction * scale,
                Y: cy + y * direction * scale,
            };
            let shaft = [point(tail, -tail), point(tip, -tip)];
            let head = [point(wing, -tip), point(tip, -tip), point(tip, -wing)];
            if gdip::GdipStartPathFigure(path) != gdip::Ok
                || gdip::GdipAddPathLine2(path, shaft.as_ptr(), shaft.len() as i32) != gdip::Ok
                || gdip::GdipStartPathFigure(path) != gdip::Ok
                || gdip::GdipAddPathLine2(path, head.as_ptr(), head.len() as i32) != gdip::Ok
            {
                return false;
            }
        }
        true
    }) {
        return;
    }
    let Some(saved) = SavedDc::new(dc) else {
        return;
    };
    let cx = (rect.left + rect.right) as f32 / 2.0;
    let cy = (rect.top + rect.bottom) as f32 / 2.0;
    let pen = CreatePen(PS_SOLID, stroke.round().max(1.0) as i32, color);
    if pen.is_null() {
        return;
    }
    SelectObject(dc, pen);
    for direction in [-1.0, 1.0] {
        let point = |x: f32, y: f32| POINT {
            x: (cx + x * direction * scale).round() as i32,
            y: (cy + y * direction * scale).round() as i32,
        };
        let shaft = [point(tail, -tail), point(tip, -tip)];
        let head = [point(wing, -tip), point(tip, -tip), point(tip, -wing)];
        Polyline(dc, shaft.as_ptr(), shaft.len() as i32);
        Polyline(dc, head.as_ptr(), head.len() as i32);
    }
    drop(saved);
    DeleteObject(pen);
}

unsafe fn draw_button_content(item: &DRAWITEMSTRUCT, style: ButtonStyle, selected: bool) -> bool {
    let Some(_saved) = SavedDc::new(item.hDC) else {
        return false;
    };
    let dpi = GetDpiForWindow(item.hwndItem).max(96);
    let disabled = item.itemState & (ODS_DISABLED | ODS_GRAYED) != 0;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let hot = !disabled && hovered(item.hwndItem);
    let focused =
        !disabled && item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
    // 名称行使用轻量图标按钮，常态不绘制外框或选中底色
    if matches!(style, ButtonStyle::NameVisibility) {
        fill(item.hDC, &item.rcItem, BACKGROUND);
        if !disabled && (hot || pressed || focused) {
            let background = if pressed { PRESSED } else { HOVER };
            rounded(
                item.hDC,
                &inset(item.rcItem, pixels(1, dpi)),
                pixels(5, dpi),
                background,
            );
        }
        let foreground = if disabled {
            DISABLED_TEXT
        } else if hot || pressed || focused {
            ACCENT
        } else {
            MUTED
        };
        // 轮廓向右靠近复选框，点击区域仍保持独立
        let icon_rect = RECT {
            left: item.rcItem.left + pixels(6, dpi),
            ..item.rcItem
        };
        name_visibility_icon(item.hDC, icon_rect, dpi, foreground, selected);
        return true;
    }
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
    } else if selected && !matches!(style, ButtonStyle::MiniMode) {
        (if pressed { PRESSED } else { ACCENT_SOFT }, ACCENT, ACCENT)
    } else if pressed {
        (PRESSED, BORDER, TEXT)
    } else if hot {
        (HOVER, BORDER, TEXT)
    } else {
        (
            SURFACE,
            BORDER,
            if matches!(
                style,
                ButtonStyle::Icon
                    | ButtonStyle::Back
                    | ButtonStyle::OpenWindow
                    | ButtonStyle::MiniMode
            ) {
                MUTED
            } else {
                TEXT
            },
        )
    };
    // 沿按钮原有圆角边框提示键盘焦点，避免叠加方形虚线
    let border = if focused {
        if matches!(style, ButtonStyle::Primary) {
            ACCENT_PRESSED
        } else {
            ACCENT
        }
    } else {
        border
    };
    fill(item.hDC, &item.rcItem, BACKGROUND);
    let outline = inset(item.rcItem, pixels(1, dpi));
    draw_rounded_surface(
        item.hDC,
        &outline,
        pixels(7, dpi),
        background,
        Some((border, pixels(1, dpi))),
    );
    let face = inset(outline, pixels(1, dpi));
    let mut content = inset(face, pixels(4, dpi));
    content.top = face.top + pixels(1, dpi);
    content.bottom = face.bottom - pixels(1, dpi);
    if picker {
        picker_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Refresh) {
        refresh_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Back) {
        back_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::OpenWindow) {
        open_window_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::Icon) {
        settings_icon(item.hDC, item.rcItem, dpi, foreground);
    } else if matches!(style, ButtonStyle::MiniMode) {
        mini_mode_icon(item.hDC, item.rcItem, dpi, foreground, selected);
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
    true
}

#[derive(Clone, Copy)]
struct TrackbarVisual {
    client: RECT,
    channel: RECT,
    thumb: RECT,
    dpi: u32,
    compact: bool,
    vertical: bool,
    enabled: bool,
    highlighted: bool,
}

pub unsafe fn install_trackbar_theme(hwnd: HWND, compact: bool) -> bool {
    SetWindowSubclass(
        hwnd,
        Some(trackbar_theme_proc),
        TRACKBAR_THEME_SUBCLASS,
        compact as usize,
    ) != 0
}

unsafe extern "system" fn trackbar_theme_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    compact: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            // 绘制仅消费已有失效区，避免在合成控件树时再次触发祖先重绘
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() && !paint_trackbar(hwnd, dc, compact != 0) {
                DefSubclassProc(hwnd, WM_PRINTCLIENT, dc as usize, PRF_CLIENT as isize);
            }
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_PRINTCLIENT => {
            if paint_trackbar(hwnd, wparam as HDC, compact != 0) {
                return 0;
            }
        }
        WM_MOUSEMOVE => {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                ..zeroed()
            };
            TrackMouseEvent(&mut tracking);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(trackbar_theme_proc), subclass);
        }
        _ => {}
    }
    // 在原生处理状态变化前扩展失效区，使同步绘制也覆盖旧进度和光晕
    let redraw = match message {
        TBM_SETPOS | TBM_SETRANGE | TBM_SETRANGEMIN | TBM_SETRANGEMAX => wparam != 0,
        WM_MOUSEMOVE
        | WM_MOUSELEAVE
        | WM_LBUTTONDOWN
        | WM_LBUTTONUP
        | WM_KEYDOWN
        | WM_KEYUP
        | WM_MOUSEWHEEL
        | WM_TIMER
        | WM_CAPTURECHANGED
        | WM_CANCELMODE
        | WM_ENABLE
        | WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_UPDATEUISTATE
        | WM_SIZE
        | WM_THEMECHANGED
        | WM_DPICHANGED_AFTERPARENT
        | TBM_SETTHUMBLENGTH
        | TBM_SETPOSNOTIFY => true,
        _ => false,
    };
    if redraw {
        InvalidateRect(hwnd, null(), 0);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe fn paint_trackbar(hwnd: HWND, dc: HDC, compact: bool) -> bool {
    let mut client: RECT = zeroed();
    if dc.is_null()
        || GetClientRect(hwnd, &mut client) == 0
        || client.right <= 0
        || client.bottom <= 0
    {
        return false;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let enabled = IsWindowEnabled(hwnd) != 0;
    let vertical = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & TBS_VERT != 0;
    let mut thumb: RECT = zeroed();
    SendMessageW(
        hwnd,
        TBM_GETTHUMBRECT,
        0,
        (&mut thumb as *mut RECT) as LPARAM,
    );
    let mut channel: RECT = zeroed();
    SendMessageW(
        hwnd,
        TBM_GETCHANNELRECT,
        0,
        &mut channel as *mut RECT as LPARAM,
    );
    let mut cursor: POINT = zeroed();
    let hot = GetCursorPos(&mut cursor) != 0
        && ScreenToClient(hwnd, &mut cursor) != 0
        && PtInRect(&thumb, cursor) != 0;
    let keyboard_focus = GetFocus() == hwnd
        && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) as u32 & UISF_HIDEFOCUS == 0;
    let visual = TrackbarVisual {
        client,
        channel,
        thumb,
        dpi,
        compact,
        vertical,
        enabled,
        highlighted: hot || keyboard_focus || GetCapture() == hwnd,
    };
    if let Some(buffer) = PaintBuffer::new(dc, client) {
        if draw_trackbar_frame(buffer.dc(), visual) && buffer.present(dc) {
            return true;
        }
    }
    draw_trackbar_frame(dc, visual)
}

unsafe fn draw_trackbar_frame(dc: HDC, visual: TrackbarVisual) -> bool {
    let Some(_saved) = SavedDc::new(dc) else {
        return false;
    };
    let TrackbarVisual {
        client,
        mut channel,
        thumb,
        dpi,
        compact,
        vertical,
        enabled,
        highlighted,
    } = visual;
    // 背景、轨道和手柄在同一缓冲中完成，消除擦底与分项绘制造成的闪动
    fill(dc, &client, BACKGROUND);
    let cx = (thumb.left + thumb.right) / 2;
    let cy = (thumb.top + thumb.bottom) / 2;
    let thickness = pixels(if compact { 3 } else { 4 }, dpi);
    if vertical {
        // 只取原生轨道长边，避开竖向控件返回矩形的横纵坐标差异
        let length = (channel.right - channel.left)
            .max(channel.bottom - channel.top)
            .clamp(1, client.bottom - client.top);
        channel.top = client.top + (client.bottom - client.top - length) / 2;
        channel.bottom = channel.top + length;
        channel.left = cx - thickness / 2;
        channel.right = channel.left + thickness;
    } else {
        channel.top = cy - thickness / 2;
        channel.bottom = channel.top + thickness;
    }
    rounded(dc, &channel, thickness, TRACK);
    if enabled && vertical && cy < channel.bottom {
        channel.top = cy.max(channel.top);
        rounded(dc, &channel, thickness, ACCENT);
    } else if enabled && !vertical && cx > channel.left {
        channel.right = cx.min(channel.right);
        rounded(dc, &channel, thickness, ACCENT);
    }
    let radius = pixels(if compact { 5 } else { 8 }, dpi);
    // 主手柄和竖向手柄不超出原生命中矩形，避免可见圆边缘无法开始拖动
    let radius = if compact && !vertical {
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
    if enabled && highlighted {
        let halo = inset(circle, -pixels(2, dpi));
        rounded(dc, &halo, radius + pixels(2, dpi), ACCENT_SOFT);
    }
    draw_rounded_surface(
        dc,
        &circle,
        radius,
        SURFACE,
        Some((if enabled { ACCENT } else { DISABLED_TEXT }, pixels(2, dpi))),
    );
    true
}

const LIST_BADGE_SIDE: i32 = 11;

#[derive(Clone, Copy)]
pub(crate) enum WindowActionIcon {
    Hidden,
    Topmost,
    Container,
    Remove,
}

impl WindowActionIcon {
    // 状态标记与菜单图标共用十二单位画布，轮廓留出描边和抗锯齿所需边距
    fn lines(self) -> &'static [&'static [(f32, f32)]] {
        match self {
            Self::Hidden => &[&[(3.1, 8.9), (8.9, 3.1)]],
            Self::Topmost => &[
                &[
                    (7.27, 1.19),
                    (10.81, 4.73),
                    (9.04, 5.08),
                    (7.77, 6.35),
                    (7.41, 8.83),
                    (3.17, 4.59),
                    (5.65, 4.23),
                    (6.92, 2.96),
                    (7.27, 1.19),
                ],
                &[(5.29, 6.71), (2.96, 9.04)],
            ],
            Self::Container => &[
                &[(4.5, 3.3), (4.5, 1.7), (10.3, 1.7), (10.3, 7.5), (8.7, 7.5)],
                &[(1.7, 4.5), (7.5, 4.5), (7.5, 10.3), (1.7, 10.3), (1.7, 4.5)],
            ],
            Self::Remove => &[&[(3.0, 3.0), (9.0, 9.0)], &[(9.0, 3.0), (3.0, 9.0)]],
        }
    }
}

pub(crate) unsafe fn draw_window_action_icon(
    dc: HDC,
    rect: RECT,
    color: COLORREF,
    icon: WindowActionIcon,
) {
    let scale = (rect.right - rect.left) as f32 / 12.0;
    if scaled_icon_path_antialiased(
        dc,
        rect,
        color,
        scale,
        1.25 * scale,
        |path, cx, cy, scale| {
            let point = |(x, y): (f32, f32)| (cx + (x - 6.0) * scale, cy + (y - 6.0) * scale);
            for line in icon.lines() {
                if gdip::GdipStartPathFigure(path) != gdip::Ok {
                    return false;
                }
                for segment in line.windows(2) {
                    let (x1, y1) = point(segment[0]);
                    let (x2, y2) = point(segment[1]);
                    if gdip::GdipAddPathLine(path, x1, y1, x2, y2) != gdip::Ok {
                        return false;
                    }
                }
            }
            if matches!(icon, WindowActionIcon::Hidden)
                && (gdip::GdipStartPathFigure(path) != gdip::Ok
                    || gdip::GdipAddPathEllipse(
                        path,
                        cx - 4.1 * scale,
                        cy - 4.1 * scale,
                        8.2 * scale,
                        8.2 * scale,
                    ) != gdip::Ok)
            {
                return false;
            }
            true
        },
    ) {
        return;
    }
    // GDI+ 不可用时沿用同一浮点轮廓，保持符号含义和比例
    let Some(saved) = SavedDc::new(dc) else {
        return;
    };
    let pen = CreatePen(PS_SOLID, (1.25 * scale).round().max(1.0) as i32, color);
    if pen.is_null() {
        return;
    }
    SelectObject(dc, pen);
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    let point = |(x, y): (f32, f32)| POINT {
        x: (rect.left as f32 + x * scale).round() as i32,
        y: (rect.top as f32 + y * scale).round() as i32,
    };
    for line in icon.lines() {
        let start = point(line[0]);
        MoveToEx(dc, start.x, start.y, null_mut());
        for &position in &line[1..] {
            let end = point(position);
            LineTo(dc, end.x, end.y);
        }
    }
    if matches!(icon, WindowActionIcon::Hidden) {
        let first = point((1.9, 1.9));
        let last = point((10.1, 10.1));
        Ellipse(dc, first.x, first.y, last.x, last.y);
    }
    drop(saved);
    DeleteObject(pen);
}

unsafe fn draw_list_name(draw: &NMLVCUSTOMDRAW, badges: WindowBadges) -> bool {
    let list = draw.nmcd.hdr.hwndFrom;
    let row = draw.nmcd.dwItemSpec;
    let dc = draw.nmcd.hdc;
    let images = SendMessageW(list, LVM_GETIMAGELIST, LVSIL_SMALL as usize, 0) as HIMAGELIST;
    let mut bounds = RECT {
        left: LVIR_BOUNDS as i32,
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
    {
        return false;
    }
    // DrawTextExW 的省略处理最多额外写入四个字符
    let mut text = [0_u16; 1028];
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
    let dpi = GetDpiForWindow(list).max(96);
    let mut icon_width = 0;
    let mut icon_height = 0;
    let has_icon = images != 0
        && item.iImage >= 0
        && ImageList_GetIconSize(images, &mut icon_width, &mut icon_height) != 0;
    if has_icon {
        ImageList_Draw(
            images,
            item.iImage,
            dc,
            cell.left + pixels(4, dpi),
            cell.top + (cell.bottom - cell.top - icon_height) / 2,
            ILD_TRANSPARENT
                | if item.state & LVIS_CUT != 0 {
                    ILD_BLEND50
                } else {
                    0
                },
        );
    }
    let gap = pixels(2, dpi);
    let side = pixels(LIST_BADGE_SIDE, dpi).min((cell.bottom - cell.top).max(1));
    let badge_width = [badges.topmost, badges.container, badges.hidden]
        .into_iter()
        .filter(|enabled| *enabled)
        .count() as i32
        * (side + gap);
    // 关闭应用图标时不保留图标宽度
    let text_left = cell.left + pixels(4, dpi);
    let mut content = RECT {
        left: if has_icon {
            text_left + icon_width + pixels(4, dpi)
        } else {
            text_left
        },
        right: cell.right - pixels(4, dpi) - badge_width,
        ..cell
    };
    if content.right <= content.left || length <= 0 {
        // 窄列或空名称仍保留已绘制的原色图标，避免原生选中绘制再次覆盖
        return true;
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
        return true;
    }
    if !badges.any() {
        return true;
    }
    let shown_length = text.iter().position(|value| *value == 0).unwrap_or(1024);
    let mut extent: SIZE = zeroed();
    if GetTextExtentPoint32W(dc, text.as_ptr(), shown_length as i32, &mut extent) == 0 {
        return true;
    }
    // 状态符号紧贴名称末端横向排列，关闭的状态不占位
    let mut left =
        (content.left + extent.cx + gap).min(cell.right - pixels(4, dpi) - badge_width + gap);
    let top = cell.top + (cell.bottom - cell.top - side) / 2;
    for (enabled, icon) in [
        (badges.topmost, WindowActionIcon::Topmost),
        (badges.container, WindowActionIcon::Container),
        (badges.hidden, WindowActionIcon::Hidden),
    ] {
        if enabled {
            draw_window_action_icon(
                dc,
                RECT {
                    left,
                    top,
                    right: left + side,
                    bottom: top + side,
                },
                foreground,
                icon,
            );
            left += side + gap;
        }
    }
    true
}

pub unsafe fn draw_list(draw: &mut NMLVCUSTOMDRAW) -> LRESULT {
    match draw.nmcd.dwDrawStage {
        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW as LRESULT,
        CDDS_ITEMPREPAINT => {
            let state = SendMessageW(
                draw.nmcd.hdr.hwndFrom,
                LVM_GETITEMSTATE,
                draw.nmcd.dwItemSpec,
                LVIS_SELECTED as LPARAM,
            ) as u32;
            let notify_subitems = CDRF_NOTIFYSUBITEMDRAW;
            if state & LVIS_SELECTED != 0 {
                return notify_subitems as LRESULT;
            }
            draw.clrText = TEXT;
            draw.clrTextBk = SURFACE;
            (CDRF_NEWFONT | notify_subitems) as LRESULT
        }
        stage if stage == CDDS_ITEMPREPAINT | CDDS_SUBITEM && draw.iSubItem == 0 => {
            let badges = window_badges(draw.nmcd.lItemlParam as usize);
            if draw_list_name(draw, badges) {
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

#[cfg(test)]
mod rendering_tests {
    use super::*;
    use std::sync::Mutex;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS,
    };

    // 所有画布均为内存 DIB，不创建窗口或获取桌面 DC
    static GDI_TEST_LOCK: Mutex<()> = Mutex::new(());
    const SENTINEL: COLORREF = rgb(217, 31, 183);

    struct Canvas {
        dc: HDC,
        bitmap: HBITMAP,
        previous_bitmap: HGDIOBJ,
        bits: *mut u32,
        width: i32,
        height: i32,
    }

    impl Canvas {
        unsafe fn new(width: i32, height: i32, background: COLORREF) -> Self {
            assert!(width > 0 && height > 0);
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null(), "CreateCompatibleDC failed");
            let mut canvas = Self {
                dc,
                bitmap: null_mut(),
                previous_bitmap: null_mut(),
                bits: null_mut(),
                width,
                height,
            };
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..zeroed()
                },
                ..zeroed()
            };
            let mut bits = null_mut();
            canvas.bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!canvas.bitmap.is_null(), "CreateDIBSection failed");
            assert!(!bits.is_null(), "DIB pixels are missing");
            canvas.bits = bits.cast();
            canvas.previous_bitmap = SelectObject(dc, canvas.bitmap);
            assert!(!canvas.previous_bitmap.is_null(), "SelectObject failed");
            fill(dc, &canvas.rect(), background);
            canvas
        }

        fn rect(&self) -> RECT {
            RECT {
                left: 0,
                top: 0,
                right: self.width,
                bottom: self.height,
            }
        }

        unsafe fn pixels(&self) -> Vec<u32> {
            assert_ne!(GdiFlush(), 0, "GDI drawing did not finish");
            std::slice::from_raw_parts(self.bits, (self.width * self.height) as usize)
                .iter()
                .map(|pixel| pixel & 0x00ff_ffff)
                .collect()
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            unsafe {
                if !self.previous_bitmap.is_null() {
                    SelectObject(self.dc, self.previous_bitmap);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.dc);
            }
        }
    }

    fn dib_color(color: COLORREF) -> u32 {
        ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff)
    }

    fn inside(rect: RECT, x: i32, y: i32) -> bool {
        x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
    }

    fn trackbar_visual(length: i32, dpi: u32, vertical: bool, compact: bool) -> TrackbarVisual {
        let side = pixels(32, dpi);
        let length = pixels(length, dpi);
        let half = pixels(9, dpi);
        let center = length / 3;
        let (client, thumb, channel) = if vertical {
            (
                RECT {
                    right: side,
                    bottom: length,
                    ..unsafe { zeroed() }
                },
                RECT {
                    left: side / 2 - half,
                    top: center - half,
                    right: side / 2 + half,
                    bottom: center + half,
                },
                RECT {
                    left: 0,
                    top: 0,
                    right: length - pixels(20, dpi),
                    bottom: pixels(4, dpi),
                },
            )
        } else {
            (
                RECT {
                    right: length,
                    bottom: side,
                    ..unsafe { zeroed() }
                },
                RECT {
                    left: center - half,
                    top: side / 2 - half,
                    right: center + half,
                    bottom: side / 2 + half,
                },
                RECT {
                    left: pixels(10, dpi),
                    top: 0,
                    right: length - pixels(10, dpi),
                    bottom: pixels(4, dpi),
                },
            )
        };
        TrackbarVisual {
            client,
            thumb,
            channel,
            dpi,
            vertical,
            compact,
            enabled: true,
            highlighted: true,
        }
    }

    #[test]
    fn trackbar_resize_replaces_old_thumb_and_background_in_one_buffer() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        unsafe {
            for dpi in [96, 144, 192] {
                for vertical in [false, true] {
                    for compact in [false, true] {
                        let canvas = Canvas::new(pixels(240, dpi), pixels(240, dpi), SENTINEL);
                        for length in [220, 80, 150, 220] {
                            let mut visual = trackbar_visual(length, dpi, vertical, compact);
                            visual.enabled = length != 80;
                            let reference = Canvas::new(canvas.width, canvas.height, SENTINEL);
                            assert!(draw_trackbar_frame(reference.dc, visual));
                            {
                                let buffer = PaintBuffer::new(canvas.dc, visual.client)
                                    .expect("buffer creation failed");
                                assert!(draw_trackbar_frame(buffer.dc(), visual));
                                assert!(buffer.present(canvas.dc));
                            }
                            let actual = canvas.pixels();
                            let expected = reference.pixels();
                            for y in visual.client.top..visual.client.bottom {
                                for x in visual.client.left..visual.client.right {
                                    let index = (y * canvas.width + x) as usize;
                                    assert_ne!(
                                        actual[index],
                                        dib_color(SENTINEL),
                                        "unpainted slider pixel"
                                    );
                                    assert_eq!(
                                        actual[index], expected[index],
                                        "old thumb or channel remained at ({x}, {y})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn trackbar_partial_updates_reconstruct_the_same_complete_frame() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        unsafe {
            for vertical in [false, true] {
                let visual = trackbar_visual(160, 144, vertical, false);
                let reference = Canvas::new(240, 240, SENTINEL);
                let partial = Canvas::new(240, 240, SENTINEL);
                assert!(draw_trackbar_frame(reference.dc, visual));
                let middle = visual.client.right / 2;
                for clip in [
                    RECT {
                        right: middle,
                        ..visual.client
                    },
                    RECT {
                        left: middle,
                        ..visual.client
                    },
                ] {
                    let _saved = SavedDc::new(partial.dc).expect("SaveDC failed");
                    assert_ne!(
                        IntersectClipRect(partial.dc, clip.left, clip.top, clip.right, clip.bottom),
                        ERROR
                    );
                    let buffer = PaintBuffer::new(partial.dc, visual.client)
                        .expect("buffer creation failed");
                    assert!(draw_trackbar_frame(buffer.dc(), visual));
                    assert!(buffer.present(partial.dc));
                }
                assert_eq!(partial.pixels(), reference.pixels());
            }
        }
    }

    #[test]
    fn all_button_styles_match_direct_drawing_after_repeated_resizing() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _graphics = GdiPlusSession::new().expect("GDI+ startup failed");
        unsafe {
            for (name, style) in [
                ("picker", ButtonStyle::Picker),
                ("refresh", ButtonStyle::Refresh),
                ("eye", ButtonStyle::NameVisibility),
                ("mini", ButtonStyle::MiniMode),
                ("back", ButtonStyle::Back),
                ("primary", ButtonStyle::Primary),
                ("secondary", ButtonStyle::Secondary),
                ("settings", ButtonStyle::Icon),
            ] {
                for size in [32, 24, 44, 32] {
                    for state in [0, ODS_DISABLED, ODS_SELECTED] {
                        let reference = Canvas::new(80, 72, SENTINEL);
                        let buffered = Canvas::new(80, 72, SENTINEL);
                        let rect = RECT {
                            left: 13,
                            top: 9,
                            right: 13 + size,
                            bottom: 9 + size,
                        };
                        let item = DRAWITEMSTRUCT {
                            CtlType: ODT_BUTTON,
                            itemAction: ODA_DRAWENTIRE,
                            itemState: state,
                            hDC: reference.dc,
                            rcItem: rect,
                            ..zeroed()
                        };
                        assert!(draw_button_content(&item, style, true));
                        let item = DRAWITEMSTRUCT {
                            hDC: buffered.dc,
                            ..item
                        };
                        draw_button(&item, style, true);
                        let expected = reference.pixels();
                        assert_eq!(
                            buffered.pixels(),
                            expected,
                            "{name}, size={size}, state={state}"
                        );
                        let mut ink = 0;
                        for y in 0..reference.height {
                            for x in 0..reference.width {
                                let pixel = expected[(y * reference.width + x) as usize];
                                if inside(rect, x, y) {
                                    assert_ne!(
                                        pixel,
                                        dib_color(SENTINEL),
                                        "unpainted {name} at ({x}, {y})"
                                    );
                                    ink += usize::from(pixel != dib_color(BACKGROUND));
                                } else {
                                    assert_eq!(
                                        pixel,
                                        dib_color(SENTINEL),
                                        "{name} painted outside its bounds"
                                    );
                                }
                            }
                        }
                        assert!(ink > 0, "{name} disappeared");
                    }
                }
            }
        }
    }

    #[test]
    fn partial_button_repaints_reconstruct_the_complete_image() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _graphics = GdiPlusSession::new().expect("GDI+ startup failed");
        unsafe {
            for style in [
                ButtonStyle::Picker,
                ButtonStyle::Refresh,
                ButtonStyle::Icon,
                ButtonStyle::MiniMode,
            ] {
                let reference = Canvas::new(64, 56, SENTINEL);
                let partial = Canvas::new(64, 56, SENTINEL);
                let rect = RECT {
                    left: 11,
                    top: 7,
                    right: 43,
                    bottom: 39,
                };
                let item = DRAWITEMSTRUCT {
                    CtlType: ODT_BUTTON,
                    itemAction: ODA_DRAWENTIRE,
                    hDC: reference.dc,
                    rcItem: rect,
                    ..zeroed()
                };
                draw_button(&item, style, false);
                let item = DRAWITEMSTRUCT {
                    hDC: partial.dc,
                    ..item
                };
                for clip in [RECT { right: 27, ..rect }, RECT { left: 27, ..rect }] {
                    let _saved = SavedDc::new(partial.dc).expect("SaveDC failed");
                    assert_ne!(
                        IntersectClipRect(partial.dc, clip.left, clip.top, clip.right, clip.bottom),
                        ERROR
                    );
                    draw_button(&item, style, false);
                }
                assert_eq!(partial.pixels(), reference.pixels());
            }
        }
    }

    #[test]
    fn paint_buffer_preserves_client_clip_with_nonzero_dirty_origin() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        unsafe {
            let canvas = Canvas::new(64, 48, SENTINEL);
            let dirty = RECT {
                left: 7,
                top: 5,
                right: 57,
                bottom: 43,
            };
            let client = RECT {
                left: 12,
                top: 13,
                right: 52,
                bottom: 38,
            };
            let marker = RECT {
                left: 10,
                top: 7,
                right: 20,
                bottom: 11,
            };
            let _saved = SavedDc::new(canvas.dc).expect("SaveDC failed");
            assert_ne!(
                ExcludeClipRect(
                    canvas.dc,
                    client.left,
                    client.top,
                    client.right,
                    client.bottom
                ),
                ERROR
            );
            {
                let buffer = PaintBuffer::new(canvas.dc, dirty).expect("buffer creation failed");
                fill(buffer.dc(), &dirty, BACKGROUND);
                fill(buffer.dc(), &marker, ACCENT);
                assert!(buffer.present(canvas.dc));
            }
            assert_eq!(
                GetCurrentObject(canvas.dc, OBJ_BITMAP as u32),
                canvas.bitmap
            );
            let pixels = canvas.pixels();
            for y in 0..canvas.height {
                for x in 0..canvas.width {
                    let expected = if !inside(dirty, x, y) || inside(client, x, y) {
                        SENTINEL
                    } else if inside(marker, x, y) {
                        ACCENT
                    } else {
                        BACKGROUND
                    };
                    assert_eq!(
                        pixels[(y * canvas.width + x) as usize],
                        dib_color(expected),
                        "at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn paint_buffer_resize_sequence_repaints_edges_and_releases_gdi_objects() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        unsafe {
            let canvas = Canvas::new(80, 60, SENTINEL);
            // 预热兼容位图路径后再检查重复扩缩时的资源数
            drop(PaintBuffer::new(canvas.dc, canvas.rect()).expect("buffer creation failed"));
            assert_ne!(GdiFlush(), 0);
            let baseline = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
            assert!(baseline > 0, "GDI resource query failed");
            for &(width, height) in [(72, 52), (24, 20), (61, 43), (38, 29)]
                .iter()
                .cycle()
                .take(32)
            {
                fill(canvas.dc, &canvas.rect(), SENTINEL);
                let bounds = RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                };
                {
                    let buffer =
                        PaintBuffer::new(canvas.dc, bounds).expect("buffer creation failed");
                    fill(buffer.dc(), &bounds, BACKGROUND);
                    fill(
                        buffer.dc(),
                        &RECT {
                            left: width - 1,
                            ..bounds
                        },
                        ACCENT,
                    );
                    fill(
                        buffer.dc(),
                        &RECT {
                            top: height - 1,
                            ..bounds
                        },
                        TEXT,
                    );
                    assert!(buffer.present(canvas.dc));
                }
                let pixels = canvas.pixels();
                for y in 0..canvas.height {
                    for x in 0..canvas.width {
                        let expected = if !inside(bounds, x, y) {
                            SENTINEL
                        } else if y == height - 1 {
                            TEXT
                        } else if x == width - 1 {
                            ACCENT
                        } else {
                            BACKGROUND
                        };
                        assert_eq!(
                            pixels[(y * canvas.width + x) as usize],
                            dib_color(expected),
                            "size {width}x{height}, at ({x}, {y})"
                        );
                    }
                }
            }
            assert_eq!(
                GetCurrentObject(canvas.dc, OBJ_BITMAP as u32),
                canvas.bitmap
            );
            assert_eq!(
                GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS),
                baseline,
                "repeated buffer allocation leaked GDI objects"
            );
        }
    }

    #[test]
    fn caption_icons_match_when_buffer_origin_is_nonzero() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _graphics = GdiPlusSession::new().expect("GDI+ startup failed");
        unsafe {
            let rect = RECT {
                left: 17,
                top: 11,
                right: 49,
                bottom: 43,
            };
            for dpi in [96, 144, 192] {
                for close in [false, true] {
                    let reference = Canvas::new(64, 56, BACKGROUND);
                    let buffered = Canvas::new(64, 56, BACKGROUND);
                    assert!(draw_caption_button_icon(
                        reference.dc,
                        rect,
                        dpi,
                        TEXT,
                        close
                    ));
                    {
                        let buffer =
                            PaintBuffer::new(buffered.dc, rect).expect("buffer creation failed");
                        fill(buffer.dc(), &rect, BACKGROUND);
                        assert!(draw_caption_button_icon(
                            buffer.dc(),
                            rect,
                            dpi,
                            TEXT,
                            close
                        ));
                        assert!(buffer.present(buffered.dc));
                    }
                    let expected = reference.pixels();
                    assert!(
                        expected.iter().any(|&pixel| pixel != dib_color(BACKGROUND)),
                        "the reference icon is empty"
                    );
                    assert_eq!(buffered.pixels(), expected, "dpi={dpi}, close={close}");
                }
            }
        }
    }

    #[test]
    fn caption_icon_partial_clip_preserves_pixels_outside_clip() {
        let _serial = GDI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _graphics = GdiPlusSession::new().expect("GDI+ startup failed");
        unsafe {
            let reference = Canvas::new(64, 48, BACKGROUND);
            let clipped = Canvas::new(64, 48, BACKGROUND);
            let rect = RECT {
                left: 20,
                top: 8,
                right: 44,
                bottom: 32,
            };
            let clip = RECT { right: 32, ..rect };
            assert!(draw_caption_button_icon(
                reference.dc,
                rect,
                144,
                TEXT,
                true
            ));
            {
                let _saved = SavedDc::new(clipped.dc).expect("SaveDC failed");
                assert_ne!(
                    IntersectClipRect(clipped.dc, clip.left, clip.top, clip.right, clip.bottom),
                    ERROR
                );
                assert!(draw_caption_button_icon(clipped.dc, rect, 144, TEXT, true));
            }
            let reference_pixels = reference.pixels();
            let clipped_pixels = clipped.pixels();
            let mut visible_ink = 0;
            for y in 0..clipped.height {
                for x in 0..clipped.width {
                    let index = (y * clipped.width + x) as usize;
                    let expected = if inside(clip, x, y) {
                        reference_pixels[index]
                    } else {
                        dib_color(BACKGROUND)
                    };
                    visible_ink += usize::from(expected != dib_color(BACKGROUND));
                    assert_eq!(clipped_pixels[index], expected, "at ({x}, {y})");
                }
            }
            assert!(visible_ink > 0, "the clipped icon is empty");
        }
    }
}
