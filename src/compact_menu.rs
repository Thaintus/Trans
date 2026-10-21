use std::cell::{Cell, RefCell};
use std::mem::{size_of, zeroed};
use std::ptr::null;
use std::rc::Rc;

use crate::localization::tr;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static ACTIVE_MENUS: RefCell<Vec<Rc<MenuContext>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ITEM_TOKEN: Cell<usize> = const { Cell::new(0) };
}

struct OwnedMenu(HMENU);

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}

struct OwnedFont(HFONT);

impl Drop for OwnedFont {
    fn drop(&mut self) {
        unsafe { DeleteObject(self.0) };
    }
}

struct WindowDc {
    owner: HWND,
    dc: HDC,
}

impl Drop for WindowDc {
    fn drop(&mut self) {
        unsafe { ReleaseDC(self.owner, self.dc) };
    }
}

struct SavedDc {
    dc: HDC,
    state: i32,
}

impl SavedDc {
    unsafe fn new(dc: HDC) -> Option<Self> {
        let state = SaveDC(dc);
        (state != 0).then_some(Self { dc, state })
    }
}

impl Drop for SavedDc {
    fn drop(&mut self) {
        unsafe { RestoreDC(self.dc, self.state) };
    }
}

struct MenuEntry {
    token: usize,
    command: u32,
    text: Vec<u16>,
    text_length: i32,
}

struct MenuContext {
    menu: OwnedMenu,
    font: OwnedFont,
    entries: Vec<MenuEntry>,
    width: u32,
    height: u32,
    padding: i32,
}

struct ActiveMenu(Rc<MenuContext>);

impl ActiveMenu {
    fn new(menu: Rc<MenuContext>) -> Self {
        ACTIVE_MENUS.with(|active| active.borrow_mut().push(Rc::clone(&menu)));
        Self(menu)
    }
}

impl Drop for ActiveMenu {
    fn drop(&mut self) {
        ACTIVE_MENUS.with(|active| {
            active
                .borrow_mut()
                .retain(|menu| !Rc::ptr_eq(menu, &self.0));
        });
    }
}

fn next_item_token() -> usize {
    loop {
        let token = NEXT_ITEM_TOKEN.with(|next| {
            let token = next.get().wrapping_add(1).max(1);
            next.set(token);
            token
        });
        let in_use = ACTIVE_MENUS.with(|active| {
            active
                .borrow()
                .iter()
                .any(|menu| menu.entries.iter().any(|entry| entry.token == token))
        });
        if !in_use {
            return token;
        }
    }
}

fn find_entry(token: usize, command: u32) -> Option<(Rc<MenuContext>, usize)> {
    if token == 0 {
        return None;
    }
    // 只匹配当前原生菜单循环内登记的整数标识，不读取 itemData 指向的内存
    ACTIVE_MENUS.with(|active| {
        active.borrow().iter().rev().find_map(|menu| {
            menu.entries
                .iter()
                .position(|entry| entry.token == token && entry.command == command)
                .map(|index| (Rc::clone(menu), index))
        })
    })
}

fn pixels(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

unsafe fn win32_error(chinese: &str, english: &str) -> String {
    format!("{} {}", tr(chinese, english), GetLastError())
}

pub(crate) unsafe fn show(
    owner: HWND,
    items: &[(u32, &str)],
    point: POINT,
    disabled_commands: &[u32],
) -> Result<u32, String> {
    if items.is_empty()
        || items.iter().any(|(command, label)| {
            *command == 0
                || label
                    .chars()
                    .any(|character| matches!(character, '\0' | '\r' | '\n' | '\t'))
        })
    {
        return Err(tr("无效的窗口菜单项", "Invalid window menu item").into());
    }
    let raw_menu = CreatePopupMenu();
    if raw_menu.is_null() {
        return Err(win32_error(
            "无法创建菜单，Windows 错误码",
            "Cannot create the menu; Windows error",
        ));
    }
    let menu = OwnedMenu(raw_menu);
    let menu_info = MENUINFO {
        cbSize: size_of::<MENUINFO>() as u32,
        fMask: MIM_STYLE | MIM_BACKGROUND,
        dwStyle: MNS_NOCHECK,
        hbrBack: GetSysColorBrush(COLOR_MENU),
        ..zeroed()
    };
    if SetMenuInfo(menu.0, &menu_info) == 0 {
        return Err(win32_error(
            "无法配置菜单，Windows 错误码",
            "Cannot configure the menu; Windows error",
        ));
    }
    let dpi = GetDpiForWindow(owner);
    let dpi = if dpi == 0 { 96 } else { dpi };
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..zeroed()
    };
    if SystemParametersInfoForDpi(
        SPI_GETNONCLIENTMETRICS,
        metrics.cbSize,
        &mut metrics as *mut NONCLIENTMETRICSW as *mut _,
        0,
        dpi,
    ) == 0
    {
        return Err(win32_error(
            "无法读取菜单字体，Windows 错误码",
            "Cannot read the menu font; Windows error",
        ));
    }
    let raw_font = CreateFontIndirectW(&metrics.lfMenuFont);
    if raw_font.is_null() {
        return Err(tr("无法创建菜单字体", "Cannot create the menu font").into());
    }
    let font = OwnedFont(raw_font);
    let raw_dc = GetDC(owner);
    if raw_dc.is_null() {
        return Err(tr(
            "无法获取菜单测量画布",
            "Cannot acquire the menu measurement context",
        )
        .into());
    }
    let dc = WindowDc { owner, dc: raw_dc };
    let saved = SavedDc::new(dc.dc).ok_or_else(|| {
        tr(
            "无法保存菜单测量状态",
            "Cannot save the menu measurement state",
        )
        .to_owned()
    })?;
    let old_font = SelectObject(dc.dc, font.0);
    if old_font.is_null() || old_font as isize == -1 {
        return Err(tr("无法选用菜单字体", "Cannot select the menu font").into());
    }
    let mut text_metrics: TEXTMETRICW = zeroed();
    if GetTextMetricsW(dc.dc, &mut text_metrics) == 0 {
        return Err(tr("无法测量菜单字体", "Cannot measure the menu font").into());
    }
    let mut entries = Vec::with_capacity(items.len());
    let mut text_width = 0;
    for &(command, label) in items {
        let mut text: Vec<u16> = label.encode_utf16().collect();
        let length = i32::try_from(text.len())
            .map_err(|_| tr("菜单文本过长", "The menu text is too long").to_owned())?;
        let mut extent: SIZE = zeroed();
        if GetTextExtentPoint32W(dc.dc, text.as_ptr(), length, &mut extent) == 0 {
            return Err(tr("无法测量菜单文本", "Cannot measure the menu text").into());
        }
        text_width = text_width.max(extent.cx);
        text.push(0);
        entries.push(MenuEntry {
            token: next_item_token(),
            command,
            text,
            text_length: length,
        });
    }
    drop(saved);
    drop(dc);
    let padding = pixels(8, dpi);
    let context = Rc::new(MenuContext {
        menu,
        font,
        entries,
        width: (text_width + 2 * padding) as u32,
        height: (text_metrics.tmHeight + 2 * pixels(4, dpi)) as u32,
        padding,
    });
    // 在 Win32 回调之前登记，在原生菜单循环完全退出后再释放字体和文本
    let _active = ActiveMenu::new(Rc::clone(&context));
    for (position, entry) in context.entries.iter().enumerate() {
        let item = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_ID | MIIM_DATA | MIIM_STRING | MIIM_STATE,
            fType: MFT_OWNERDRAW,
            fState: if disabled_commands.contains(&entry.command) {
                MFS_GRAYED
            } else {
                MFS_ENABLED
            },
            wID: entry.command,
            dwItemData: entry.token,
            dwTypeData: entry.text.as_ptr() as *mut u16,
            cch: entry.text_length as u32,
            ..zeroed()
        };
        if InsertMenuItemW(context.menu.0, position as u32, 1, &item) == 0 {
            return Err(win32_error(
                "无法创建菜单项，Windows 错误码",
                "Cannot create the menu item; Windows error",
            ));
        }
    }
    SetForegroundWindow(owner);
    let command = TrackPopupMenuEx(
        context.menu.0,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        owner,
        null(),
    );
    PostMessageW(owner, WM_NULL, 0, 0);
    Ok(command as u32)
}

pub(crate) unsafe fn measure_item(item: &mut MEASUREITEMSTRUCT) -> bool {
    if item.CtlType != ODT_MENU {
        return false;
    }
    let Some((context, _)) = find_entry(item.itemData, item.itemID) else {
        return false;
    };
    item.itemWidth = context.width;
    item.itemHeight = context.height;
    true
}

pub(crate) unsafe fn draw_item(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_MENU {
        return false;
    }
    let Some((context, index)) = find_entry(item.itemData, item.itemID) else {
        return false;
    };
    if item.hwndItem as HMENU != context.menu.0 {
        return false;
    }
    let Some(_saved) = SavedDc::new(item.hDC) else {
        return true;
    };
    let disabled = item.itemState & (ODS_DISABLED | ODS_GRAYED) != 0;
    let selected = item.itemState & ODS_SELECTED != 0 && !disabled;
    let background = if selected {
        COLOR_HIGHLIGHT
    } else {
        COLOR_MENU
    };
    let foreground = if disabled {
        COLOR_GRAYTEXT
    } else if selected {
        COLOR_HIGHLIGHTTEXT
    } else {
        COLOR_MENUTEXT
    };
    FillRect(item.hDC, &item.rcItem, GetSysColorBrush(background));
    SelectObject(item.hDC, context.font.0);
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SetTextColor(item.hDC, GetSysColor(foreground));
    let entry = &context.entries[index];
    let mut text_rect = item.rcItem;
    text_rect.left += context.padding;
    text_rect.right -= context.padding;
    DrawTextW(
        item.hDC,
        entry.text.as_ptr(),
        entry.text_length,
        &mut text_rect,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
    true
}
