use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ptr::null_mut;
use std::rc::Rc;
use std::time::Instant;

use windows_sys::Win32::Foundation::{GetLastError, SetLastError, HANDLE, HWND};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Controls::{
    ImageList_Create, ImageList_Destroy, ImageList_GetImageCount, ImageList_Remove,
    ImageList_ReplaceIcon, HIMAGELIST, ILC_COLOR32, ILC_MASK,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassLongPtrW, GetPropW, GetWindowThreadProcessId, IsWindow, LoadIconW, RemovePropW,
    SendMessageTimeoutW, SetPropW, GCLP_HICON, GCLP_HICONSM, HICON, ICON_BIG, ICON_SMALL,
    ICON_SMALL2, IDI_APPLICATION, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, WM_GETICON,
};

use crate::localization::tr;
use crate::window_control::WindowInfo;

const ALPHABET: &[u8; 36] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const ICON_TIMEOUT_MS: u32 = 30;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct WindowKey {
    hwnd: usize,
    process_id: u32,
    thread_id: u32,
}

impl WindowKey {
    fn of(window: &WindowInfo) -> Self {
        Self {
            hwnd: window.hwnd as usize,
            process_id: window.process_id,
            thread_id: window.thread_id,
        }
    }

    fn matches(self) -> bool {
        let hwnd = self.hwnd as HWND;
        let mut process_id = 0;
        unsafe {
            !hwnd.is_null()
                && IsWindow(hwnd) != 0
                && GetWindowThreadProcessId(hwnd, &mut process_id) == self.thread_id
                && self.thread_id != 0
                && process_id == self.process_id
                && process_id != 0
                && process_id != GetCurrentProcessId()
        }
    }
}

struct PrivateName {
    cookie: usize,
    title: String,
}

pub(crate) struct PrivacyNames {
    property_name: Vec<u16>,
    names: HashMap<WindowKey, PrivateName>,
    used_names: HashSet<String>,
    next_cookie: usize,
}

impl PrivacyNames {
    pub(crate) fn new() -> Result<Self, String> {
        let mut nonce = [0u8; 16];
        random_bytes(&mut nonce)?;
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02X}")).collect();
        let property_name = format!("Trans.Privacy.{}.{nonce}", unsafe { GetCurrentProcessId() })
            .encode_utf16()
            .chain(Some(0))
            .collect();
        Ok(Self {
            property_name,
            names: HashMap::new(),
            used_names: HashSet::new(),
            next_cookie: 1,
        })
    }

    // 独立 cookie 在未登记到已登记的切换中保持不变，不依赖控制器 token
    pub(crate) fn title(&mut self, window: &WindowInfo, _token: usize) -> Result<String, String> {
        let key = WindowKey::of(window);
        if !key.matches() {
            return Err(tr(
                "窗口身份已变化，无法生成隐私名称",
                "The window identity changed; a private name cannot be generated",
            )
            .into());
        }
        if let Some(name) = self.names.get(&key) {
            let cookie = unsafe { GetPropW(window.hwnd, self.property_name.as_ptr()) };
            if cookie as usize == name.cookie && key.matches() {
                return Ok(name.title.clone());
            }
        }
        let title = self.unused_name()?;
        let cookie = self.next_cookie;
        self.next_cookie = cookie.checked_add(1).ok_or(tr(
            "隐私窗口标记已耗尽，无法生成隐私名称",
            "Privacy markers are exhausted; a private name cannot be generated",
        ))?;
        if !key.matches() {
            return Err(tr(
                "窗口身份已变化，无法生成隐私名称",
                "The window identity changed; a private name cannot be generated",
            )
            .into());
        }
        unsafe {
            SetLastError(0);
            if SetPropW(window.hwnd, self.property_name.as_ptr(), cookie as HANDLE) == 0 {
                return Err(format!(
                    "{} {}",
                    tr(
                        "无法设置隐私窗口标记，Windows 错误码",
                        "Cannot set the privacy marker; Windows error"
                    ),
                    GetLastError()
                ));
            }
            if !key.matches()
                || GetPropW(window.hwnd, self.property_name.as_ptr()) as usize != cookie
            {
                return Err(tr("窗口身份或隐私标记已变化，无法生成隐私名称", "The window identity or privacy marker changed; a private name cannot be generated").into());
            }
        }
        self.used_names.insert(title.clone());
        self.names.insert(
            key,
            PrivateName {
                cookie,
                title: title.clone(),
            },
        );
        Ok(title)
    }

    fn unused_name(&self) -> Result<String, String> {
        for _ in 0..64 {
            let title = random_name()?;
            if !self.used_names.contains(&title) {
                return Ok(title);
            }
        }
        Err(tr(
            "无法生成唯一隐私名称",
            "Cannot generate a unique private name",
        )
        .into())
    }
}

impl Drop for PrivacyNames {
    fn drop(&mut self) {
        for (key, name) in &self.names {
            if key.matches() {
                unsafe {
                    let hwnd = key.hwnd as HWND;
                    if GetPropW(hwnd, self.property_name.as_ptr()) as usize == name.cookie
                        && key.matches()
                    {
                        RemovePropW(hwnd, self.property_name.as_ptr());
                    }
                }
            }
        }
    }
}

struct CachedIcon {
    token: usize,
    index: i32,
}

// 控件应设置 LVS_SHAREIMAGELISTS，替换或销毁前先解除对该列表的引用
pub(crate) struct IconList {
    handle: HIMAGELIST,
    dpi: u32,
    icons: HashMap<WindowKey, CachedIcon>,
    free_indices: Vec<i32>,
    _thread: PhantomData<Rc<()>>,
}

impl IconList {
    pub(crate) fn new(dpi: u32) -> Result<Self, String> {
        let dpi = dpi.clamp(96, 768);
        let size = ((16 * dpi + 48) / 96) as i32;
        let handle = unsafe { ImageList_Create(size, size, ILC_COLOR32 | ILC_MASK, 1, 8) };
        if handle == 0 {
            return Err(tr("无法创建窗口图标列表", "Cannot create the window icon list").into());
        }
        let list = Self {
            handle,
            dpi,
            icons: HashMap::new(),
            free_indices: Vec::new(),
            _thread: PhantomData,
        };
        let icon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
        if icon.is_null() || unsafe { ImageList_ReplaceIcon(handle, -1, icon) } != 0 {
            return Err(tr(
                "无法加载默认窗口图标",
                "Cannot load the default window icon",
            )
            .into());
        }
        Ok(list)
    }

    pub(crate) fn image_list(&self) -> HIMAGELIST {
        self.handle
    }

    pub(crate) fn icon_index(&mut self, window: &WindowInfo, token: usize) -> i32 {
        let key = WindowKey::of(window);
        if !key.matches() {
            return 0;
        }
        if let Some(cached) = self.icons.get_mut(&key) {
            if compatible_tokens(cached.token, token) {
                if token != 0 {
                    cached.token = token;
                }
                return cached.index;
            }
        }
        self.remove_cached(key);
        let icon = window_icon(window, self.dpi);
        let index = if icon.is_null() || !key.matches() {
            0
        } else {
            let free = self.free_indices.pop();
            // ImageList_AddIcon 是 ImageList_ReplaceIcon(list, -1, icon) 的 SDK 宏
            let index = unsafe { ImageList_ReplaceIcon(self.handle, free.unwrap_or(-1), icon) };
            if index < 0 {
                if let Some(index) = free {
                    self.free_indices.push(index);
                }
                0
            } else {
                index
            }
        };
        if key.matches() {
            self.icons.insert(key, CachedIcon { token, index });
        } else if index > 0 {
            self.free_indices.push(index);
            return 0;
        }
        index
    }

    // 同步全部正在展示的窗口集合，不改变保留窗口的图标索引
    pub(crate) fn sync<'a, I>(&mut self, windows: I)
    where
        I: IntoIterator<Item = (&'a WindowInfo, usize)>,
    {
        let live: HashMap<_, _> = windows
            .into_iter()
            .map(|(window, token)| (WindowKey::of(window), token))
            .collect();
        let stale: Vec<_> = self
            .icons
            .iter()
            .filter(|(key, icon)| {
                !key.matches()
                    || !live
                        .get(*key)
                        .is_some_and(|token| compatible_tokens(icon.token, *token))
            })
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.remove_cached(key);
        }
        self.trim_unused_tail();
    }

    fn remove_cached(&mut self, key: WindowKey) {
        if let Some(icon) = self.icons.remove(&key) {
            if icon.index > 0 {
                self.free_indices.push(icon.index);
            }
        }
    }

    fn trim_unused_tail(&mut self) {
        loop {
            let last = unsafe { ImageList_GetImageCount(self.handle) } - 1;
            if last <= 0 {
                break;
            }
            let Some(position) = self.free_indices.iter().position(|index| *index == last) else {
                break;
            };
            if unsafe { ImageList_Remove(self.handle, last) } == 0 {
                break;
            }
            self.free_indices.swap_remove(position);
        }
    }
}

impl Drop for IconList {
    fn drop(&mut self) {
        unsafe { ImageList_Destroy(self.handle) };
    }
}

fn compatible_tokens(left: usize, right: usize) -> bool {
    left == 0 || right == 0 || left == right
}

fn random_bytes(bytes: &mut [u8]) -> Result<(), String> {
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        Err(format!(
            "{} 0x{:08X}",
            tr(
                "无法取得系统随机数，NTSTATUS",
                "Cannot obtain system random bytes; NTSTATUS"
            ),
            status as u32
        ))
    } else {
        Ok(())
    }
}

fn random_name() -> Result<String, String> {
    let mut name = String::with_capacity(7);
    for _ in 0..64 {
        let mut bytes = [0u8; 16];
        random_bytes(&mut bytes)?;
        for byte in bytes {
            // 丢弃末尾 4 个取值，避免 36 个字符出现取模偏差
            if byte < 252 {
                name.push(ALPHABET[(byte % 36) as usize] as char);
                if name.len() == 7 {
                    return Ok(name);
                }
            }
        }
    }
    Err(tr(
        "系统随机数未能生成隐私名称",
        "System random bytes could not produce a private name",
    )
    .into())
}

fn window_icon(window: &WindowInfo, dpi: u32) -> HICON {
    let key = WindowKey::of(window);
    let started = Instant::now();
    for size in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
        if !key.matches() {
            return null_mut();
        }
        let elapsed = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        let remaining = ICON_TIMEOUT_MS.saturating_sub(elapsed);
        if remaining == 0 {
            break;
        }
        let mut icon = 0;
        let result = unsafe {
            SendMessageTimeoutW(
                window.hwnd,
                WM_GETICON,
                size as usize,
                dpi as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
                remaining,
                &mut icon,
            )
        };
        if !key.matches() {
            return null_mut();
        }
        if result == 0 {
            break;
        }
        if icon != 0 {
            return icon as HICON;
        }
    }
    for slot in [GCLP_HICONSM, GCLP_HICON] {
        if !key.matches() {
            return null_mut();
        }
        let icon = unsafe { GetClassLongPtrW(window.hwnd, slot) };
        if !key.matches() {
            return null_mut();
        }
        if icon != 0 {
            return icon as HICON;
        }
    }
    null_mut()
}
