use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    GetLastError, SetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetStockObject, InvalidateRect, SetDCBrushColor, UpdateWindow,
    DC_BRUSH, HBRUSH, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, RegisterClassExW,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GA_ROOT, GWLP_USERDATA, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA,
    MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOZORDER,
    SWP_SHOWWINDOW, SW_HIDE, WM_DPICHANGED, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_DISABLED, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::localization::tr;
use crate::window_control::WindowInfo;

const CLASS_NAME: &str = "Trans.PickerOverlay.1";
const ACCENT: u32 = 0x00d96c26;
static CLASS_REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);

struct ShownTarget {
    hwnd: HWND,
    process_id: u32,
    thread_id: u32,
    bounds: RECT,
    border_dpi: [u32; 4],
}

pub struct PickerOverlay {
    borders: [HWND; 4],
    token: usize,
    thread_id: u32,
    shown: Option<ShownTarget>,
}

impl PickerOverlay {
    pub fn new(owner: HWND) -> Result<Self, String> {
        unsafe {
            let mut owner_process = 0;
            let owner_thread = GetWindowThreadProcessId(owner, &mut owner_process);
            if owner.is_null()
                || IsWindow(owner) == 0
                || owner_process != GetCurrentProcessId()
                || owner_thread != GetCurrentThreadId()
            {
                return Err(tr("无法创建选窗轮廓：主窗口身份已失效或不属于当前界面线程", "Cannot create the selection outline: the main window is invalid or belongs to another UI thread").into());
            }
            register_class()?;
            let instance = GetModuleHandleW(null());
            if instance.is_null() {
                return Err(api_error(tr(
                    "无法读取选窗轮廓模块",
                    "Cannot access the selection outline module",
                )));
            }
            let token = loop {
                let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
                if token != 0 {
                    break token;
                }
            };
            let mut overlay = Self {
                borders: [null_mut(); 4],
                token,
                thread_id: owner_thread,
                shown: None,
            };
            let class = wide(CLASS_NAME);
            for index in 0..overlay.borders.len() {
                let border = CreateWindowExW(
                    WS_EX_NOACTIVATE
                        | WS_EX_TOOLWINDOW
                        | WS_EX_TOPMOST
                        | WS_EX_LAYERED
                        | WS_EX_TRANSPARENT,
                    class.as_ptr(),
                    null(),
                    // WindowFromPoint 会跳过禁用窗口，轮廓不能成为下一次悬停目标
                    WS_POPUP | WS_DISABLED,
                    0,
                    0,
                    1,
                    1,
                    owner,
                    null_mut(),
                    instance,
                    token as *const c_void,
                );
                if border.is_null() {
                    return Err(api_error(tr(
                        "无法创建选窗轮廓边框",
                        "Cannot create the selection outline border",
                    )));
                }
                overlay.borders[index] = border;
                if SetLayeredWindowAttributes(border, 0, 255, LWA_ALPHA) == 0 {
                    return Err(api_error(tr(
                        "无法设置选窗轮廓显示方式",
                        "Cannot configure the selection outline display",
                    )));
                }
            }
            Ok(overlay)
        }
    }

    pub fn show(&mut self, target: &WindowInfo) -> Result<(), String> {
        let result = self.show_target(target);
        if result.is_err() {
            self.hide();
        }
        result
    }

    fn show_target(&mut self, target: &WindowInfo) -> Result<(), String> {
        validate_target(target)?;
        if self.borders.iter().any(|border| !self.owns(*border)) {
            return Err(tr(
                "选窗轮廓窗口已失效，请结束拖动后重新选择",
                "The selection outline is invalid; stop dragging and select again",
            )
            .into());
        }
        let bounds = target_bounds(target.hwnd)?;
        validate_target(target)?;
        let width = bounds
            .right
            .checked_sub(bounds.left)
            .filter(|size| *size > 0);
        let height = bounds
            .bottom
            .checked_sub(bounds.top)
            .filter(|size| *size > 0);
        let (Some(width), Some(height)) = (width, height) else {
            return Err(tr(
                "目标窗口没有可用的外框区域",
                "The target window has no usable frame area",
            )
            .into());
        };
        if self.shown.as_ref().is_some_and(|shown| {
            shown.hwnd == target.hwnd
                && shown.process_id == target.process_id
                && shown.thread_id == target.thread_id
                && same_bounds(&shown.bounds, &bounds)
                && self
                    .borders
                    .iter()
                    .zip(shown.border_dpi)
                    .all(|(border, dpi)| unsafe {
                        IsWindowVisible(*border) != 0 && GetDpiForWindow(*border) == dpi
                    })
        }) {
            return Ok(());
        }

        self.hide();
        unsafe {
            // 读取叠加层所在屏幕的 DPI，避免目标窗口不感知 DPI 时返回固定 96
            if SetWindowPos(
                self.borders[0],
                null_mut(),
                bounds.left + width / 2,
                bounds.top + height / 2,
                1,
                1,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING,
            ) == 0
            {
                return Err(api_error(tr(
                    "无法定位选窗轮廓",
                    "Cannot position the selection outline",
                )));
            }
            let dpi = GetDpiForWindow(self.borders[0]);
            if dpi == 0 {
                return Err(tr(
                    "无法读取选窗轮廓所在屏幕的显示比例",
                    "Cannot read the display scale for the selection outline",
                )
                .into());
            }
            let thickness =
                ((3_u64 * dpi as u64 + 48) / 96).clamp(1, width.min(height) as u64) as i32;
            let positions = [
                (bounds.left, bounds.top, width, thickness),
                (bounds.left, bounds.bottom - thickness, width, thickness),
                (bounds.left, bounds.top, thickness, height),
                (bounds.right - thickness, bounds.top, thickness, height),
            ];
            validate_target(target)?;
            for (border, (x, y, width, height)) in self.borders.iter().zip(positions) {
                if !self.owns(*border) {
                    return Err(tr(
                        "选窗轮廓窗口已失效，请结束拖动后重新选择",
                        "The selection outline is invalid; stop dragging and select again",
                    )
                    .into());
                }
                if SetWindowPos(
                    *border,
                    HWND_TOPMOST,
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING | SWP_SHOWWINDOW,
                ) == 0
                {
                    return Err(api_error(tr(
                        "无法显示选窗轮廓",
                        "Cannot show the selection outline",
                    )));
                }
                if InvalidateRect(*border, null(), 0) == 0 || UpdateWindow(*border) == 0 {
                    return Err(api_error(tr(
                        "无法绘制选窗轮廓",
                        "Cannot draw the selection outline",
                    )));
                }
            }
        }
        validate_target(target)?;
        let border_dpi = self
            .borders
            .map(|border| unsafe { GetDpiForWindow(border) });
        if border_dpi.contains(&0) {
            return Err(tr(
                "选窗轮廓窗口已失效，请结束拖动后重新选择",
                "The selection outline is invalid; stop dragging and select again",
            )
            .into());
        }
        self.shown = Some(ShownTarget {
            hwnd: target.hwnd,
            process_id: target.process_id,
            thread_id: target.thread_id,
            bounds,
            border_dpi,
        });
        Ok(())
    }

    pub fn hide(&mut self) {
        self.shown = None;
        for border in self.borders {
            if self.owns(border) {
                unsafe { ShowWindow(border, SW_HIDE) };
            }
        }
    }

    fn owns(&self, border: HWND) -> bool {
        unsafe {
            let mut process_id = 0;
            !border.is_null()
                && GetWindowThreadProcessId(border, &mut process_id) == self.thread_id
                && process_id == GetCurrentProcessId()
                && GetWindowLongPtrW(border, GWLP_USERDATA) as usize == self.token
        }
    }
}

impl Drop for PickerOverlay {
    fn drop(&mut self) {
        self.hide();
        for border in self.borders {
            if self.owns(border) {
                unsafe { DestroyWindow(border) };
            }
        }
    }
}

fn register_class() -> Result<(), String> {
    CLASS_REGISTERED
        .get_or_init(|| unsafe {
            let instance = GetModuleHandleW(null());
            if instance.is_null() {
                return Err(api_error(tr(
                    "无法读取选窗轮廓模块",
                    "Cannot access the selection outline module",
                )));
            }
            let name = wide(CLASS_NAME);
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_proc),
                hInstance: instance,
                lpszClassName: name.as_ptr(),
                ..zeroed()
            };
            if RegisterClassExW(&class) == 0 {
                Err(api_error(tr(
                    "无法注册选窗轮廓窗口类",
                    "Cannot register the selection outline window class",
                )))
            } else {
                Ok(())
            }
        })
        .clone()
}

fn validate_target(target: &WindowInfo) -> Result<(), String> {
    unsafe {
        let mut process_id = 0;
        if target.hwnd.is_null()
            || IsWindow(target.hwnd) == 0
            || GetWindowThreadProcessId(target.hwnd, &mut process_id) != target.thread_id
            || process_id != target.process_id
            || process_id == GetCurrentProcessId()
            || GetAncestor(target.hwnd, GA_ROOT) != target.hwnd
            || IsWindowVisible(target.hwnd) == 0
            || IsIconic(target.hwnd) != 0
        {
            return Err(tr(
                "悬停目标已关闭、隐藏或身份发生变化",
                "The hovered window has closed, is hidden, or has changed identity",
            )
            .into());
        }
        let mut cloaked = 0_u32;
        if DwmGetWindowAttribute(
            target.hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        ) >= 0
            && cloaked != 0
        {
            return Err(tr(
                "悬停目标当前不在可见桌面上",
                "The hovered window is not on the visible desktop",
            )
            .into());
        }
        Ok(())
    }
}

fn target_bounds(hwnd: HWND) -> Result<RECT, String> {
    unsafe {
        let mut bounds: RECT = zeroed();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut bounds as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        ) >= 0
            && bounds.right > bounds.left
            && bounds.bottom > bounds.top
        {
            return Ok(bounds);
        }
        if GetWindowRect(hwnd, &mut bounds) == 0 {
            return Err(api_error(tr(
                "无法读取悬停目标外框",
                "Cannot read the hovered window frame",
            )));
        }
        Ok(bounds)
    }
}

fn same_bounds(left: &RECT, right: &RECT) -> bool {
    left.left == right.left
        && left.top == right.top
        && left.right == right.right
        && left.bottom == right.bottom
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn api_error(operation: &str) -> String {
    format!(
        "{operation}{} {}",
        tr("，Windows 错误码", "; Windows error"),
        unsafe { GetLastError() }
    )
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetLastError(0);
            if SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) == 0
                && GetLastError() != 0
            {
                return 0;
            }
            return 1;
        }
        WM_NCHITTEST => return HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => return 1,
        // 轮廓使用屏幕像素坐标，显示时自行计算尺寸，不采用系统建议的缩放矩形
        WM_DPICHANGED => return 0,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() {
                SetDCBrushColor(dc, ACCENT);
                FillRect(dc, &paint.rcPaint, GetStockObject(DC_BRUSH) as HBRUSH);
            }
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}
