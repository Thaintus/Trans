// 嵌入与顶层联动容器共用标题栏和边距，目标窗口关系由 window_container 维护
use std::cell::Cell;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT,
    TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const BACKGROUND: u32 = 0x00f9_f4ef;
const BORDER: u32 = 0x00b9_b9b9;
const TEXT: u32 = 0x0020_2020;
const HOVER: u32 = 0x00ed_e8e3;
const PRESSED: u32 = 0x00d5_d0cb;
const CLOSE_HOVER: u32 = 0x0023_11c4;

pub struct ContainerFrame {
    hover: Cell<u32>,
    pressed: Cell<u32>,
}

impl Default for ContainerFrame {
    fn default() -> Self {
        Self {
            hover: Cell::new(HTNOWHERE),
            pressed: Cell::new(HTNOWHERE),
        }
    }
}

fn scaled(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(96) as i64 + 48) / 96) as i32
}

unsafe fn caption_height(dpi: u32) -> i32 {
    // 沿用系统标题栏总高度，将原先的厚边距纳入平面标题栏
    (GetSystemMetricsForDpi(SM_CYCAPTION, dpi)
        + GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi)
        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi))
    .max(scaled(24, dpi))
}

pub unsafe fn expand_client(rect: &mut RECT, dpi: u32) {
    rect.left -= 1;
    rect.right += 1;
    rect.top -= caption_height(dpi);
    rect.bottom += 1;
}

struct Layout {
    window: RECT,
    width: i32,
    height: i32,
    caption: i32,
    button: i32,
    dpi: u32,
}

impl Layout {
    unsafe fn read(host: HWND) -> Option<Self> {
        let mut window: RECT = zeroed();
        if GetWindowRect(host, &mut window) == 0 {
            return None;
        }
        let width = window.right - window.left;
        let height = window.bottom - window.top;
        if width < 2 || height < 2 {
            return None;
        }
        let dpi = GetDpiForWindow(host).max(96);
        Some(Self {
            window,
            width,
            height,
            caption: caption_height(dpi).min(height - 1),
            button: scaled(46, dpi).min((width - 2) / 3).max(1),
            dpi,
        })
    }

    fn button_rect(&self, hit: u32) -> RECT {
        let index = match hit {
            HTCLOSE => 1,
            HTMAXBUTTON => 2,
            _ => 3,
        };
        RECT {
            left: self.width - 1 - index * self.button,
            top: 1,
            right: self.width - 1 - (index - 1) * self.button,
            bottom: self.caption,
        }
    }

    unsafe fn hit(&self, host: HWND, point: POINT) -> u32 {
        let x = point.x - self.window.left;
        let y = point.y - self.window.top;
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return HTNOWHERE;
        }
        if IsZoomed(host) == 0 {
            let corner = scaled(6, self.dpi);
            if y < 1 {
                return if x < corner {
                    HTTOPLEFT
                } else if x >= self.width - corner {
                    HTTOPRIGHT
                } else {
                    HTTOP
                };
            }
            if y >= self.height - 1 {
                return if x < corner {
                    HTBOTTOMLEFT
                } else if x >= self.width - corner {
                    HTBOTTOMRIGHT
                } else {
                    HTBOTTOM
                };
            }
            if x < 1 {
                return HTLEFT;
            }
            if x >= self.width - 1 {
                return HTRIGHT;
            }
        }
        if y >= self.caption {
            return HTCLIENT;
        }
        for hit in [HTCLOSE, HTMAXBUTTON, HTMINBUTTON] {
            if x >= self.button_rect(hit).left {
                return hit;
            }
        }
        if x < scaled(32, self.dpi) {
            HTSYSMENU
        } else {
            HTCAPTION
        }
    }
}

fn is_button(hit: u32) -> bool {
    matches!(hit, HTMINBUTTON | HTMAXBUTTON | HTCLOSE)
}

unsafe fn fill(dc: HDC, rect: RECT, color: u32) {
    SetDCBrushColor(dc, color);
    FillRect(dc, &rect, GetStockObject(DC_BRUSH));
}

unsafe fn line(dc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) {
    MoveToEx(dc, x1, y1, null_mut());
    LineTo(dc, x2, y2);
}

impl ContainerFrame {
    unsafe fn paint(&self, host: HWND) {
        let Some(layout) = Layout::read(host) else {
            return;
        };
        let dc = GetWindowDC(host);
        if dc.is_null() {
            return;
        }
        let saved = SaveDC(dc);
        if saved == 0 {
            ReleaseDC(host, dc);
            return;
        }
        // 不写入透明客户区，也不碰原应用的绘制表面
        ExcludeClipRect(dc, 1, layout.caption, layout.width - 1, layout.height - 1);
        fill(
            dc,
            RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.height,
            },
            BORDER,
        );
        fill(
            dc,
            RECT {
                left: 1,
                top: 1,
                right: layout.width - 1,
                bottom: layout.caption,
            },
            BACKGROUND,
        );
        let mut metrics: NONCLIENTMETRICSW = zeroed();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let font = if SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            &mut metrics as *mut _ as _,
            0,
            layout.dpi,
        ) != 0
        {
            CreateFontIndirectW(&metrics.lfCaptionFont)
        } else {
            null_mut()
        };
        SelectObject(
            dc,
            if font.is_null() {
                GetStockObject(DEFAULT_GUI_FONT)
            } else {
                font
            },
        );
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, TEXT);
        let icon_size = GetSystemMetricsForDpi(SM_CXSMICON, layout.dpi).max(1);
        let left = scaled(8, layout.dpi);
        let icon = SendMessageW(host, WM_GETICON, ICON_SMALL as usize, 0) as HICON;
        if !icon.is_null() {
            DrawIconEx(
                dc,
                left,
                (layout.caption - icon_size) / 2,
                icon,
                icon_size,
                icon_size,
                0,
                null_mut(),
                DI_NORMAL,
            );
        }
        let mut title = [0u16; 1024];
        let length = GetWindowTextW(host, title.as_mut_ptr(), title.len() as i32);
        let mut text_rect = RECT {
            left: left + icon_size + scaled(6, layout.dpi),
            top: 1,
            right: layout.button_rect(HTMINBUTTON).left - scaled(6, layout.dpi),
            bottom: layout.caption,
        };
        if text_rect.right > text_rect.left {
            DrawTextW(
                dc,
                title.as_ptr(),
                length,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        for hit in [HTMINBUTTON, HTMAXBUTTON, HTCLOSE] {
            let rect = layout.button_rect(hit);
            let hot = self.hover.get() == hit;
            let background = if hot && hit == HTCLOSE {
                CLOSE_HOVER
            } else if hot && self.pressed.get() == hit {
                PRESSED
            } else if hot {
                HOVER
            } else {
                BACKGROUND
            };
            fill(dc, rect, background);
            let pen = CreatePen(
                PS_SOLID,
                scaled(1, layout.dpi).max(1),
                if hot && hit == HTCLOSE {
                    0x00ff_ffff
                } else {
                    TEXT
                },
            );
            if pen.is_null() {
                continue;
            }
            let old = SelectObject(dc, pen);
            let size = scaled(9, layout.dpi).max(5);
            let x = (rect.left + rect.right - size) / 2;
            let y = (rect.top + rect.bottom - size) / 2;
            match hit {
                HTMINBUTTON => line(dc, x, y + size / 2, x + size, y + size / 2),
                HTMAXBUTTON if IsZoomed(host) != 0 => {
                    let offset = scaled(3, layout.dpi);
                    Rectangle(dc, x + offset, y, x + size, y + size - offset);
                    fill(
                        dc,
                        RECT {
                            left: x,
                            top: y + offset,
                            right: x + size - offset,
                            bottom: y + size,
                        },
                        background,
                    );
                    Rectangle(dc, x, y + offset, x + size - offset, y + size);
                }
                HTMAXBUTTON => {
                    Rectangle(dc, x, y, x + size, y + size);
                }
                _ => {
                    line(dc, x, y, x + size, y + size);
                    line(dc, x + size - 1, y, x - 1, y + size);
                }
            }
            SelectObject(dc, old);
            DeleteObject(pen);
        }
        RestoreDC(dc, saved);
        if !font.is_null() {
            DeleteObject(font);
        }
        ReleaseDC(host, dc);
    }

    unsafe fn cursor_hit(&self, host: HWND) -> u32 {
        let mut point: POINT = zeroed();
        if GetCursorPos(&mut point) == 0 {
            return HTNOWHERE;
        }
        Layout::read(host).map_or(HTNOWHERE, |layout| layout.hit(host, point))
    }

    pub unsafe fn process(
        &self,
        host: HWND,
        message: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_NCCALCSIZE if lp != 0 => {
                let rect = if wp != 0 {
                    &mut (*(lp as *mut NCCALCSIZE_PARAMS)).rgrc[0]
                } else {
                    &mut *(lp as *mut RECT)
                };
                rect.left = (rect.left + 1).min(rect.right);
                rect.right = (rect.right - 1).max(rect.left);
                rect.bottom = (rect.bottom - 1).max(rect.top);
                rect.top =
                    (rect.top + caption_height(GetDpiForWindow(host).max(96))).min(rect.bottom);
                return Some(0);
            }
            WM_GETMINMAXINFO if lp != 0 => {
                let monitor = MonitorFromWindow(host, MONITOR_DEFAULTTONEAREST);
                let mut info: MONITORINFO = zeroed();
                info.cbSize = size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(monitor, &mut info) != 0 {
                    let bounds = &mut *(lp as *mut MINMAXINFO);
                    bounds.ptMaxPosition = POINT {
                        x: info.rcWork.left - info.rcMonitor.left,
                        y: info.rcWork.top - info.rcMonitor.top,
                    };
                    bounds.ptMaxSize = POINT {
                        x: info.rcWork.right - info.rcWork.left,
                        y: info.rcWork.bottom - info.rcWork.top,
                    };
                }
                return Some(0);
            }
            WM_NCPAINT | WM_NCACTIVATE => {
                self.paint(host);
                return Some(if message == WM_NCACTIVATE { 1 } else { 0 });
            }
            WM_NCHITTEST => {
                let point = POINT {
                    x: (lp as u16 as i16) as i32,
                    y: ((lp as u32 >> 16) as u16 as i16) as i32,
                };
                return Layout::read(host).map(|layout| layout.hit(host, point) as LRESULT);
            }
            WM_GETTITLEBARINFOEX if lp != 0 => {
                let info = &mut *(lp as *mut TITLEBARINFOEX);
                if info.cbSize == size_of::<TITLEBARINFOEX>() as u32 {
                    if let Some(layout) = Layout::read(host) {
                        info.rcTitleBar = RECT {
                            left: layout.window.left + 1,
                            top: layout.window.top + 1,
                            right: layout.window.right - 1,
                            bottom: layout.window.top + layout.caption,
                        };
                        info.rgstate = [0; 6];
                        info.rgrect = [zeroed(); 6];
                        // 保留标准标题栏元素索引，预留项和帮助按钮不可见
                        info.rgstate[1] = 0x8001;
                        info.rgstate[4] = 0x8001;
                        info.rgrect[0] = info.rcTitleBar;
                        for (index, hit) in [(2, HTMINBUTTON), (3, HTMAXBUTTON), (5, HTCLOSE)] {
                            let mut rect = layout.button_rect(hit);
                            rect.left += layout.window.left;
                            rect.right += layout.window.left;
                            rect.top += layout.window.top;
                            rect.bottom += layout.window.top;
                            info.rgrect[index] = rect;
                            if self.pressed.get() == hit {
                                info.rgstate[index] |= 8;
                            }
                        }
                    }
                    return Some(0);
                }
            }
            WM_NCMOUSEMOVE => {
                let hit = wp as u32;
                if self.hover.replace(hit) != hit {
                    self.paint(host);
                }
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: host,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tracking);
                return Some(0);
            }
            WM_NCMOUSELEAVE => {
                self.hover.set(HTNOWHERE);
                self.paint(host);
                return Some(0);
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if is_button(wp as u32) => {
                self.pressed.set(wp as u32);
                self.hover.set(wp as u32);
                SetCapture(host);
                if GetCapture() != host {
                    self.pressed.set(HTNOWHERE);
                }
                self.paint(host);
                return Some(0);
            }
            WM_MOUSEMOVE if self.pressed.get() != HTNOWHERE => {
                let hit = self.cursor_hit(host);
                if self.hover.replace(hit) != hit {
                    self.paint(host);
                }
                return Some(0);
            }
            WM_LBUTTONUP if self.pressed.get() != HTNOWHERE => {
                let pressed = self.pressed.replace(HTNOWHERE);
                let hit = self.cursor_hit(host);
                if GetCapture() == host {
                    ReleaseCapture();
                }
                self.hover.set(hit);
                self.paint(host);
                if pressed == hit {
                    let command = match hit {
                        HTMINBUTTON => SC_MINIMIZE,
                        HTMAXBUTTON if IsZoomed(host) != 0 => SC_RESTORE,
                        HTMAXBUTTON => SC_MAXIMIZE,
                        _ => SC_CLOSE,
                    };
                    PostMessageW(host, WM_SYSCOMMAND, command as usize, 0);
                }
                return Some(0);
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                if self.pressed.replace(HTNOWHERE) != HTNOWHERE {
                    self.hover.set(HTNOWHERE);
                    if message == WM_CANCELMODE && GetCapture() == host {
                        ReleaseCapture();
                    }
                    self.paint(host);
                    return Some(0);
                }
            }
            WM_SETTEXT | WM_SETICON | WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_SETTINGCHANGE => {
                let result = DefWindowProcW(host, message, wp, lp);
                self.paint(host);
                return Some(result);
            }
            _ => {}
        }
        None
    }
}
