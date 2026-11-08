use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::rc::Rc;

use crate::localization::tr;
use crate::ui_theme::{self, WindowActionIcon};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static ACTIVE_MENUS: RefCell<Vec<Rc<MenuContext>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ITEM_TOKEN: Cell<usize> = const { Cell::new(0) };
    static APPLYING_MENU_CORNERS: Cell<bool> = const { Cell::new(false) };
}

struct OwnedMenu(HMENU);

struct MenuDpiProbe(HWND);

impl Drop for MenuDpiProbe {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyWindow(self.0) };
        }
    }
}

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
        (state != 0).then(|| Self { dc, state })
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
    icon: Option<WindowActionIcon>,
}

struct MenuContext {
    menu: OwnedMenu,
    font: OwnedFont,
    entries: Vec<MenuEntry>,
    dpi: u32,
    width: u32,
    height: u32,
    padding: i32,
    icon_size: i32,
    icon_slot_width: i32,
    corners: RefCell<Option<MenuCorners>>,
    hiding_hwnd: Cell<HWND>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MenuWindowBounds {
    hwnd: HWND,
    width: i32,
    height: i32,
    dpi: u32,
}

struct OwnedRegion(HRGN);

impl Drop for OwnedRegion {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteObject(self.0) };
        }
    }
}

struct MenuCorners {
    bounds: MenuWindowBounds,
    original_region: Option<OwnedRegion>,
    desired_region: OwnedRegion,
    scratch_region: OwnedRegion,
    modified: bool,
}

#[derive(Clone, Copy)]
enum MenuCornerEvent {
    Refresh,
    Show,
    Hide,
}

struct MenuCornerScope;

impl MenuCornerScope {
    fn enter() -> Option<Self> {
        APPLYING_MENU_CORNERS.with(|applying| {
            if applying.replace(true) {
                None
            } else {
                Some(Self)
            }
        })
    }
}

impl Drop for MenuCornerScope {
    fn drop(&mut self) {
        APPLYING_MENU_CORNERS.with(|applying| applying.set(false));
    }
}

struct MenuCornerHook {
    before: HHOOK,
    after: HHOOK,
    _thread_bound: PhantomData<*mut ()>,
}

impl MenuCornerHook {
    unsafe fn new() -> Self {
        Self {
            before: SetWindowsHookExW(
                WH_CALLWNDPROC,
                Some(menu_corner_hook),
                null_mut(),
                GetCurrentThreadId(),
            ),
            after: SetWindowsHookExW(
                WH_CALLWNDPROCRET,
                Some(menu_corner_return_hook),
                null_mut(),
                GetCurrentThreadId(),
            ),
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for MenuCornerHook {
    fn drop(&mut self) {
        if !self.after.is_null() {
            unsafe { UnhookWindowsHookEx(self.after) };
        }
        if !self.before.is_null() {
            unsafe { UnhookWindowsHookEx(self.before) };
        }
    }
}

unsafe fn popup_menu_handle(hwnd: HWND) -> Option<HMENU> {
    if hwnd.is_null() || GetWindowThreadProcessId(hwnd, null_mut()) != GetCurrentThreadId() {
        return None;
    }
    let mut class = [0u16; 16];
    let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    if length != 6 || class[..6] != [35, 51, 50, 55, 54, 56] {
        return None;
    }
    let mut info = MENUBARINFO {
        cbSize: size_of::<MENUBARINFO>() as u32,
        ..zeroed()
    };
    (GetMenuBarInfo(hwnd, OBJID_CLIENT, 0, &mut info) != 0 && !info.hMenu.is_null())
        .then_some(info.hMenu)
}

unsafe fn create_menu_region(bounds: MenuWindowBounds) -> Option<OwnedRegion> {
    let diameter = (pixels(4, bounds.dpi) * 2)
        .max(1)
        .min(bounds.width)
        .min(bounds.height);
    let region = OwnedRegion(CreateRoundRectRgn(
        0,
        0,
        bounds.width,
        bounds.height,
        diameter,
        diameter,
    ));
    (!region.0.is_null()).then_some(region)
}

unsafe fn set_menu_region(corners: &mut MenuCorners) -> bool {
    // 原生菜单可能在尺寸不变时重设区域，只有实际形状相同才跳过
    if GetWindowRgn(corners.bounds.hwnd, corners.scratch_region.0) != 0
        && EqualRgn(corners.scratch_region.0, corners.desired_region.0) != 0
    {
        return true;
    }
    // 缓存区域始终由本程序持有，只将独立副本交给系统
    let region = OwnedRegion(CreateRectRgn(0, 0, 0, 0));
    if region.0.is_null()
        || CombineRgn(region.0, corners.desired_region.0, null_mut(), RGN_COPY) == 0
        || SetWindowRgn(corners.bounds.hwnd, region.0, 1) == 0
    {
        return false;
    }
    // 成功后区域归系统所有，不能再释放或访问该 HRGN
    std::mem::forget(region);
    corners.modified = true;
    true
}

unsafe fn restore_menu_corners(context: &MenuContext, corners: &mut MenuCorners) -> bool {
    if !corners.modified || popup_menu_handle(corners.bounds.hwnd) != Some(context.menu.0) {
        return true;
    }
    let region = corners
        .original_region
        .as_ref()
        .map_or(null_mut(), |region| region.0);
    if SetWindowRgn(corners.bounds.hwnd, region, 0) != 0 {
        if let Some(region) = &mut corners.original_region {
            region.0 = null_mut();
        }
        corners.modified = false;
        return true;
    }
    false
}

unsafe fn clear_menu_corners(context: &MenuContext) {
    let Some(_scope) = MenuCornerScope::enter() else {
        return;
    };
    let corners = context.corners.borrow_mut().take();
    if let Some(mut corners) = corners {
        if !restore_menu_corners(context, &mut corners) {
            context.corners.replace(Some(corners));
        }
    }
}

unsafe fn update_menu_corners(hwnd: HWND, expected_menu: Option<HMENU>, event: MenuCornerEvent) {
    // 查询菜单身份也可能发送消息，先防重入再访问任何窗口 API
    let Some(_scope) = MenuCornerScope::enter() else {
        return;
    };
    update_menu_corners_guarded(hwnd, expected_menu, event);
}

unsafe fn update_menu_corners_from_dc(dc: HDC, menu: HMENU) {
    let Some(_scope) = MenuCornerScope::enter() else {
        return;
    };
    update_menu_corners_guarded(WindowFromDC(dc), Some(menu), MenuCornerEvent::Refresh);
}

unsafe fn paint_menu_border(hwnd: HWND) {
    let Some(_scope) = MenuCornerScope::enter() else {
        return;
    };
    paint_menu_border_guarded(hwnd, None);
}

unsafe fn paint_menu_border_from_dc(dc: HDC, menu: HMENU) {
    let Some(_scope) = MenuCornerScope::enter() else {
        return;
    };
    paint_menu_border_guarded(WindowFromDC(dc), Some(menu));
}

unsafe fn paint_menu_border_guarded(hwnd: HWND, expected_menu: Option<HMENU>) {
    let Some(menu) = popup_menu_handle(hwnd) else {
        return;
    };
    if expected_menu.is_some_and(|expected| expected != menu) {
        return;
    }
    let context = ACTIVE_MENUS.with(|active| {
        active
            .borrow()
            .iter()
            .rev()
            .find(|context| context.menu.0 == menu)
            .cloned()
    });
    let Some(context) = context else {
        return;
    };
    if context.hiding_hwnd.get() == hwnd || IsWindowVisible(hwnd) == 0 {
        return;
    }
    let corners = context.corners.borrow_mut().take();
    if let Some(corners) = corners {
        if corners.bounds.hwnd == hwnd {
            draw_menu_border(&corners);
        }
        context.corners.replace(Some(corners));
    }
}

unsafe fn draw_menu_border(corners: &MenuCorners) {
    let bounds = corners.bounds;
    let mut rect: RECT = zeroed();
    if GetWindowRect(bounds.hwnd, &mut rect) == 0
        || rect.right - rect.left != bounds.width
        || rect.bottom - rect.top != bounds.height
    {
        return;
    }
    let raw_dc = GetWindowDC(bounds.hwnd);
    if raw_dc.is_null() {
        return;
    }
    let dc = WindowDc {
        owner: bounds.hwnd,
        dc: raw_dc,
    };
    let Some(_saved) = SavedDc::new(dc.dc) else {
        return;
    };
    let background = GetSysColor(COLOR_MENU);
    let shadow = GetSysColor(COLOR_3DSHADOW);
    let color = if shadow == background {
        GetSysColor(COLOR_MENUTEXT)
    } else {
        shadow
    };
    SetDCBrushColor(dc.dc, color);
    let stroke = pixels(1, bounds.dpi).max(1);
    // 沿实际裁剪区域只绘制轮廓，不填充内部以免盖住文字和选中底色
    FrameRgn(
        dc.dc,
        corners.desired_region.0,
        GetStockObject(DC_BRUSH),
        stroke,
        stroke,
    );
}

unsafe fn update_menu_corners_guarded(
    hwnd: HWND,
    expected_menu: Option<HMENU>,
    event: MenuCornerEvent,
) {
    let Some(menu) = popup_menu_handle(hwnd) else {
        return;
    };
    if expected_menu.is_some_and(|expected| expected != menu) {
        return;
    }
    let context = ACTIVE_MENUS.with(|active| {
        active
            .borrow()
            .iter()
            .rev()
            .find(|context| context.menu.0 == menu)
            .cloned()
    });
    let Some(context) = context else {
        return;
    };
    match event {
        MenuCornerEvent::Hide => context.hiding_hwnd.set(hwnd),
        MenuCornerEvent::Show => context.hiding_hwnd.set(null_mut()),
        MenuCornerEvent::Refresh if context.hiding_hwnd.get() == hwnd => return,
        MenuCornerEvent::Refresh => {}
    }
    let mut previous = context.corners.borrow_mut().take();
    if matches!(event, MenuCornerEvent::Hide) {
        if let Some(mut previous) = previous {
            if !restore_menu_corners(&context, &mut previous) {
                context.corners.replace(Some(previous));
            }
        }
        return;
    }
    if IsWindowVisible(hwnd) == 0 {
        context.corners.replace(previous);
        return;
    }
    let mut rect: RECT = zeroed();
    if GetWindowRect(hwnd, &mut rect) == 0 || rect.right <= rect.left || rect.bottom <= rect.top {
        context.corners.replace(previous);
        return;
    }
    let bounds = MenuWindowBounds {
        hwnd,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
        dpi: context.dpi,
    };
    if let Some(corners) = previous.as_mut() {
        if corners.bounds.hwnd == hwnd {
            if corners.bounds != bounds {
                let Some(region) = create_menu_region(bounds) else {
                    context.corners.replace(previous);
                    return;
                };
                corners.desired_region = region;
                corners.bounds = bounds;
            }
            set_menu_region(corners);
            context.corners.replace(previous);
            return;
        }
    }
    if let Some(mut previous) = previous {
        if !restore_menu_corners(&context, &mut previous) {
            context.corners.replace(Some(previous));
            return;
        }
    }
    let original_region = OwnedRegion(CreateRectRgn(0, 0, 0, 0));
    if original_region.0.is_null() {
        return;
    }
    let original_region = if GetWindowRgn(hwnd, original_region.0) != 0 {
        Some(original_region)
    } else {
        None
    };
    let Some(desired_region) = create_menu_region(bounds) else {
        return;
    };
    let scratch_region = OwnedRegion(CreateRectRgn(0, 0, 0, 0));
    if scratch_region.0.is_null() {
        return;
    }
    let mut corners = MenuCorners {
        bounds,
        original_region,
        desired_region,
        scratch_region,
        modified: false,
    };
    set_menu_region(&mut corners);
    context.corners.replace(Some(corners));
}

unsafe extern "system" fn menu_corner_return_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 || lparam == 0 {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    if code == HC_ACTION as i32 {
        let message = &*(lparam as *const CWPRETSTRUCT);
        match message.message {
            // 原生窗口完成布局后再按最终尺寸校正，复用同一重入保护
            WM_WINDOWPOSCHANGED if message.lParam != 0 => {
                let position = &*(message.lParam as *const WINDOWPOS);
                if position.flags & SWP_HIDEWINDOW == 0 {
                    let event = if position.flags & SWP_SHOWWINDOW != 0 {
                        MenuCornerEvent::Show
                    } else {
                        MenuCornerEvent::Refresh
                    };
                    update_menu_corners(message.hwnd, None, event);
                }
            }
            WM_SHOWWINDOW if message.wParam != 0 => {
                update_menu_corners(message.hwnd, None, MenuCornerEvent::Show);
            }
            WM_NCPAINT | WM_PAINT => paint_menu_border(message.hwnd),
            _ => {}
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn menu_corner_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || lparam == 0 {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    if code == HC_ACTION as i32 {
        let message = &*(lparam as *const CWPSTRUCT);
        match message.message {
            WM_WINDOWPOSCHANGING | WM_WINDOWPOSCHANGED if message.lParam != 0 => {
                let position = &*(message.lParam as *const WINDOWPOS);
                let hide = position.flags & SWP_HIDEWINDOW != 0;
                if hide {
                    update_menu_corners(message.hwnd, None, MenuCornerEvent::Hide);
                }
            }
            WM_SHOWWINDOW if message.wParam == 0 => {
                update_menu_corners(message.hwnd, None, MenuCornerEvent::Hide);
            }
            WM_NCPAINT | WM_PAINT => {
                update_menu_corners(message.hwnd, None, MenuCornerEvent::Refresh);
            }
            WM_NCDESTROY => {
                if let Some(_scope) = MenuCornerScope::enter() {
                    let contexts = ACTIVE_MENUS.with(|active| active.borrow().clone());
                    for context in contexts {
                        if context.hiding_hwnd.get() == message.hwnd {
                            context.hiding_hwnd.set(null_mut());
                        }
                        let removed = {
                            let mut corners = context.corners.borrow_mut();
                            if corners
                                .as_ref()
                                .is_some_and(|corners| corners.bounds.hwnd == message.hwnd)
                            {
                                corners.take()
                            } else {
                                None
                            }
                        };
                        drop(removed);
                    }
                }
            }
            _ => {}
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
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
        // 隐藏和循环结束时恢复失败，注销上下文前再作最后一次尝试
        unsafe { clear_menu_corners(&self.0) };
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

unsafe fn popup_dpi(point: POINT) -> Result<u32, String> {
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    if monitor.is_null() {
        return Err(tr(
            "无法确定菜单所在显示器",
            "Cannot determine the menu display",
        )
        .into());
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..zeroed()
    };
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        return Err(tr(
            "无法读取菜单显示器信息",
            "Cannot read the menu display information",
        )
        .into());
    }
    let rect = info.rcMonitor;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err(tr("菜单显示器范围无效", "The menu display bounds are invalid").into());
    }
    // 无 owner 的隐藏窗口按弹出位置读取 DPI，不受主窗口所在屏幕影响
    let class = [83u16, 84, 65, 84, 73, 67, 0];
    let probe = MenuDpiProbe(CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class.as_ptr(),
        null(),
        WS_POPUP,
        point.x.clamp(rect.left, rect.right - 1),
        point.y.clamp(rect.top, rect.bottom - 1),
        1,
        1,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    ));
    if probe.0.is_null() {
        return Err(win32_error(
            "无法创建菜单 DPI 查询窗口，Windows 错误码",
            "Cannot create the menu DPI query window; Windows error",
        ));
    }
    if MonitorFromWindow(probe.0, MONITOR_DEFAULTTONULL) != monitor {
        return Err(tr(
            "菜单 DPI 查询窗口未位于目标显示器",
            "The menu DPI query window is not on the target display",
        )
        .into());
    }
    let dpi = GetDpiForWindow(probe.0);
    if dpi == 0 {
        return Err(tr("无法读取菜单 DPI", "Cannot read the menu DPI").into());
    }
    Ok(dpi)
}

pub(crate) unsafe fn show(
    owner: HWND,
    items: &[(u32, &str)],
    point: POINT,
    disabled_commands: &[u32],
    icons: &[(u32, WindowActionIcon)],
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
    let dpi = popup_dpi(point)?;
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
    // 菜单字号略小于系统默认值，保留字体选择与 DPI 缩放
    metrics.lfMenuFont.lfHeight = match metrics.lfMenuFont.lfHeight {
        0 => -pixels(11, dpi),
        height if height < 0 => ((height * 11 - 6) / 12).min(-1),
        height => ((height * 11 + 6) / 12).max(1),
    };
    metrics.lfMenuFont.lfWidth = 0;
    metrics.lfMenuFont.lfQuality = CLEARTYPE_QUALITY;
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
            icon: icons
                .iter()
                .find(|(id, _)| *id == command)
                .map(|(_, icon)| *icon),
        });
    }
    drop(saved);
    drop(dc);
    let padding = pixels(4, dpi);
    let icon_size = pixels(14, dpi);
    let icon_slot_width = if entries.iter().any(|entry| entry.icon.is_some()) {
        icon_size + pixels(4, dpi)
    } else {
        0
    };
    let context = Rc::new(MenuContext {
        menu,
        font,
        entries,
        dpi,
        width: (text_width + 2 * padding + icon_slot_width) as u32,
        height: (text_metrics.tmHeight.max(icon_size) + 2 * pixels(4, dpi)) as u32,
        padding,
        icon_size,
        icon_slot_width,
        corners: RefCell::new(None),
        hiding_hwnd: Cell::new(null_mut()),
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
    let command = {
        // 钩子只在当前线程的菜单循环内存在，失败时仍保留原生菜单功能
        let _corners = MenuCornerHook::new();
        TrackPopupMenuEx(
            context.menu.0,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            owner,
            null(),
        )
    };
    clear_menu_corners(&context);
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
    if !std::ptr::eq(item.hwndItem, context.menu.0) {
        return false;
    }
    update_menu_corners_from_dc(item.hDC, context.menu.0);
    let Some(_saved) = SavedDc::new(item.hDC) else {
        return true;
    };
    let disabled = item.itemState & (ODS_DISABLED | ODS_GRAYED) != 0;
    let selected = item.itemState & ODS_SELECTED != 0 && !disabled;
    let foreground = if disabled {
        COLOR_GRAYTEXT
    } else if selected {
        COLOR_HIGHLIGHTTEXT
    } else {
        COLOR_MENUTEXT
    };
    FillRect(item.hDC, &item.rcItem, GetSysColorBrush(COLOR_MENU));
    if selected {
        let inset = pixels(1, context.dpi);
        let highlight = RECT {
            left: item.rcItem.left + inset,
            top: item.rcItem.top + inset,
            right: item.rcItem.right - inset,
            bottom: item.rcItem.bottom - inset,
        };
        if !ui_theme::draw_rounded_surface(
            item.hDC,
            &highlight,
            pixels(3, context.dpi),
            GetSysColor(COLOR_HIGHLIGHT),
            None,
        ) {
            FillRect(item.hDC, &highlight, GetSysColorBrush(COLOR_HIGHLIGHT));
        }
    }
    SelectObject(item.hDC, context.font.0);
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SetTextColor(item.hDC, GetSysColor(foreground));
    let entry = &context.entries[index];
    if let Some(icon) = entry.icon {
        let left = item.rcItem.left + context.padding;
        let top = item.rcItem.top + (item.rcItem.bottom - item.rcItem.top - context.icon_size) / 2;
        ui_theme::draw_window_action_icon(
            item.hDC,
            RECT {
                left,
                top,
                right: left + context.icon_size,
                bottom: top + context.icon_size,
            },
            GetSysColor(foreground),
            icon,
        );
    }
    let mut text_rect = item.rcItem;
    text_rect.left += context.padding + context.icon_slot_width;
    text_rect.right -= context.padding;
    DrawTextW(
        item.hDC,
        entry.text.as_ptr(),
        entry.text_length,
        &mut text_rect,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
    // 原生菜单可只重绘单项，单项完成后同步补齐外框
    paint_menu_border_from_dc(item.hDC, context.menu.0);
    true
}
