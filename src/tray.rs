use std::mem::size_of;
use std::ptr::null;

use windows_sys::Win32::Foundation::{GetLastError, SetLastError, HWND};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION_4,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{LoadIconW, WM_APP};

use crate::localization::tr;

pub const CALLBACK_MESSAGE: u32 = WM_APP + 2;
pub const TRAY_ID: u32 = 1;
const ICON_RESOURCE: usize = 101;

pub struct TrayIcon {
    hwnd: HWND,
    added: bool,
}

impl TrayIcon {
    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd, added: false }
    }

    pub unsafe fn add(&mut self) -> Result<(), String> {
        // Explorer 重启或重复添加时先清理相同窗口及 ID 的旧图标
        self.remove();
        if self.hwnd.is_null() {
            return Err(tr(
                "无法添加托盘图标：主窗口尚未创建",
                "Cannot add the tray icon: the main window has not been created",
            )
            .to_owned());
        }

        SetLastError(0);
        let instance = GetModuleHandleW(null());
        if instance.is_null() {
            return Err(api_error(
                tr("无法读取应用模块", "Cannot access the application module"),
                GetLastError(),
            ));
        }
        SetLastError(0);
        let icon = LoadIconW(instance, ICON_RESOURCE as *const u16);
        if icon.is_null() {
            return Err(api_error(
                tr("无法加载托盘图标", "Cannot load the tray icon"),
                GetLastError(),
            ));
        }

        let mut tip = [0u16; 128];
        for (destination, character) in tip.iter_mut().zip("Trans".encode_utf16()) {
            *destination = character;
        }
        let data = NOTIFYICONDATAW {
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: CALLBACK_MESSAGE,
            hIcon: icon,
            szTip: tip,
            ..self.identity()
        };
        SetLastError(0);
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            let error = GetLastError();
            self.remove();
            return Err(api_error(
                tr("无法添加托盘图标", "Cannot add the tray icon"),
                error,
            ));
        }
        self.added = true;

        let version = NOTIFYICONDATAW {
            Anonymous: NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            },
            ..self.identity()
        };
        SetLastError(0);
        if Shell_NotifyIconW(NIM_SETVERSION, &version) == 0 {
            let error = GetLastError();
            self.remove();
            return Err(api_error(
                tr("无法初始化托盘交互", "Cannot initialize tray interaction"),
                error,
            ));
        }
        Ok(())
    }

    pub unsafe fn remove(&mut self) {
        if !self.hwnd.is_null() {
            Shell_NotifyIconW(NIM_DELETE, &self.identity());
        }
        self.added = false;
    }

    pub fn is_added(&self) -> bool {
        self.added
    }

    fn identity(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..Default::default()
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            self.remove();
        }
    }
}

fn api_error(action: &str, code: u32) -> String {
    if code == 0 {
        action.to_owned()
    } else {
        format!(
            "{action}{}{}{} {code}",
            tr("：", ": "),
            std::io::Error::from_raw_os_error(code as i32),
            tr("，错误", "; error")
        )
    }
}
