use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::{
    SS_CENTER, SS_CENTERIMAGE, SS_ENDELLIPSIS, SS_LEFT, SS_NOPREFIX, SS_RIGHT,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, NINF_KEY, NIN_SELECT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::compact_menu;
use crate::hotkey::Hotkey;
use crate::localization::{set_language, tr, Language};
use crate::picker_overlay::PickerOverlay;
use crate::presentation::{IconList, PrivacyNames};
use crate::settings::{Settings, WindowGeometry, MAX_SELF_TRANSPARENCY};
use crate::settings_panel::SettingsPanel;
use crate::tray::{self, TrayIcon};
use crate::ui_theme::{self, ButtonStyle, ACCENT, MUTED, TEXT};
use crate::window_control::{self, ChangedWindowInfo, WindowController, WindowInfo};
use crate::window_events::WindowEvents;
use crate::window_search::{display_title, filter_windows};

const CLASS_NAME: &str = "Trans.MainWindow.1";
const APP_EVENT: u32 = WM_APP + 1;
const LIST_COLUMN_RESIZE: u32 = WM_APP + 3;
const TARGET_ACTIVITY_MESSAGE: u32 = WM_APP + 5;
const EVENT_LAYOUT: usize = 1;
const EVENT_PICK_START: usize = 2;
const EVENT_PICK_MOVE: usize = 3;
const EVENT_PICK_FINISH: usize = 4;
const EVENT_PICK_CANCEL: usize = 5;
const EVENT_SLIDER: usize = 6;
const EVENT_CLOSE: usize = 7;
const EVENT_DESTROY: usize = 8;
const EVENT_COMMAND: usize = 9;
const EVENT_SHOW_ERROR: usize = 10;
const EVENT_CHANGED_SELECT: usize = 11;
const EVENT_WINDOW_REFRESH: usize = 12;
const EVENT_SELF_SLIDER: usize = 13;
const EVENT_SHOW_MENU: usize = 14;
const EVENT_SHOW_MAIN: usize = 15;
const EVENT_TRAY_READD: usize = 16;
const EVENT_EXIT: usize = 17;
const EVENT_CHANGED_ACTIVATE: usize = 18;
const EVENT_HOTKEY_FOCUS: usize = 19;
const EVENT_HOTKEY_RESTORE: usize = 20;
const EVENT_WINDOW_SEARCH: usize = 21;
const EVENT_WINDOW_CHOOSE: usize = 22;
const EVENT_CLEAR_SELECTION: usize = 23;
const EVENT_DEACTIVATE: usize = 24;
const EVENT_SETTINGS_SCROLL: usize = 25;
const EVENT_LANGUAGE_CHANGE: usize = 26;
const EVENT_TARGET_ACTIVITY: usize = 27;
const EVENT_ESCAPE: usize = 28;
const EVENT_TOPMOST_COMPLETED: usize = 29;
const HOTKEY_RESTORE: i32 = 1;
const TIMER_TARGET_ACTIVITY: usize = 2;
const ICON_RESOURCE: usize = 101;
const ID_PICK: i32 = 100;
const ID_WINDOWS: i32 = 101;
const ID_REFRESH: i32 = 102;
const ID_TARGET: i32 = 103;
const ID_VALUE: i32 = 105;
const ID_SLIDER: i32 = 106;
const ID_PRESET: i32 = 110;
const ID_RESTORE: i32 = 120;
const ID_RESTORE_ALL: i32 = 121;
const ID_TOPMOST: i32 = 122;
const ID_CHANGED_TITLE: i32 = 126;
const ID_CHANGED_LIST: i32 = 128;
const ID_SETTINGS: i32 = 130;
const ID_SELF_LABEL: i32 = 131;
const ID_SELF_SLIDER: i32 = 132;
const ID_SETTINGS_TITLE: i32 = 140;
const ID_SETTINGS_BACK: i32 = 141;
const ID_CLOSE_TO_TRAY: i32 = 142;
const ID_SHOW_ICONS: i32 = 143;
const ID_PRIVACY: i32 = 144;
const ID_HOTKEY_TITLE: i32 = 145;
const ID_EXIT: i32 = 146;
const ID_HOTKEY_LABEL: i32 = 147;
const ID_HOTKEY_INPUT: i32 = 148;
const ID_HOTKEY_SAVE: i32 = 149;
const ID_HOTKEY_ENABLED: i32 = 150;
const ID_BASIC_TITLE: i32 = 151;
const ID_LANGUAGE_LABEL: i32 = 152;
const ID_LANGUAGE: i32 = 153;
const ID_DEFAULT_CONTAINER: i32 = 154;
const ID_PRESETS_TITLE: i32 = 160;
const ID_PRESETS_SAVE: i32 = 161;
const ID_PRESET_LABEL: i32 = 170;
const ID_PRESET_INPUT: i32 = 180;
const MENU_RESTORE_WINDOW: u32 = 200;
const MENU_OPEN: u32 = 201;
const MENU_SETTINGS: u32 = 202;
const MENU_RESTORE_ALL: u32 = 203;
const MENU_EXIT: u32 = 204;
const MENU_HIDE_TASKBAR: u32 = 205;
const MENU_SHOW_TASKBAR: u32 = 206;
const MENU_ENABLE_CONTAINER: u32 = 207;
const MENU_DISABLE_CONTAINER: u32 = 208;
const MENU_TOPMOST_WINDOW: u32 = 209;
const MENU_CANCEL_TOPMOST: u32 = 210;
// windows-sys 未导出该别名，Windows SDK 将 TBM_GETPOS 定义为 WM_USER
const TBM_GETPOS: u32 = WM_USER;
const WIDTH: i32 = 340;
const HEIGHT: i32 = 364;
const MIN_WIDTH: i32 = 320;
const MIN_MAIN_BODY_HEIGHT: i32 = 262;
const DEFAULT_HEADER_HEIGHT: i32 = 18;
const MAIN_STYLE: u32 = (WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX) | WS_CLIPCHILDREN;
const BUTTON_SELECTED_PROP: &str = "Trans.Ui.Selected";

thread_local! {
    static DEFERRED_EVENTS: RefCell<VecDeque<(HWND, usize, isize)>> = const { RefCell::new(VecDeque::new()) };
    static INTERACTION_VERSION: Cell<usize> = const { Cell::new(0) };
    static PICKER_VERSION: Cell<usize> = const { Cell::new(0) };
    static PICKER_CAPTURE: Cell<usize> = const { Cell::new(0) };
    static PICK_FINISH: RefCell<Option<(Option<WindowInfo>, usize)>> = const { RefCell::new(None) };
    static SEARCH_VERSION: Cell<usize> = const { Cell::new(0) };
    static SEARCH_COMPOSING: Cell<bool> = const { Cell::new(false) };
    static SEARCH_COMPOSITION_VERSION: Cell<usize> = const { Cell::new(0) };
    static COMBO_UPDATING: Cell<bool> = const { Cell::new(false) };
    static LAYOUT_PENDING: Cell<bool> = const { Cell::new(false) };
    static LAYOUT_IN_PROGRESS: Cell<bool> = const { Cell::new(false) };
    static LIST_COLUMNS_SIZING: Cell<bool> = const { Cell::new(false) };
    static LIST_COLUMN_WEIGHTS: Cell<[u32; 3]> = const { Cell::new([3, 1, 1]) };
    static LIST_COLUMN_REQUEST: Cell<Option<(usize, i32)>> = const { Cell::new(None) };
    static SEARCH_REQUEST: RefCell<Option<SearchRequest>> = const { RefCell::new(None) };
    static WINDOW_OPTIONS: RefCell<Vec<WindowInfo>> = const { RefCell::new(Vec::new()) };
    static WINDOW_CHOICE: RefCell<Option<WindowInfo>> = const { RefCell::new(None) };
}

struct SearchRequest {
    query: Option<String>,
    open: bool,
    refresh: bool,
}

fn search_version() -> usize {
    SEARCH_VERSION.with(Cell::get)
}

fn search_composing() -> bool {
    SEARCH_COMPOSING.with(Cell::get)
}

fn combo_updating() -> bool {
    COMBO_UPDATING.with(Cell::get)
}

fn set_combo_updating(value: bool) {
    COMBO_UPDATING.with(|updating| updating.set(value));
}

fn invalidate_window_search() {
    SEARCH_VERSION.with(|version| version.set(version.get().wrapping_add(1).max(1)));
    SEARCH_REQUEST.with(|request| {
        request.borrow_mut().take();
    });
}

unsafe fn queue_window_search(hwnd: HWND, open: bool, refresh: bool, query: Option<String>) {
    let previous = SEARCH_REQUEST.with(|request| request.borrow_mut().take());
    invalidate_window_search();
    SEARCH_REQUEST.with(|request| {
        *request.borrow_mut() = Some(SearchRequest {
            query: query.or_else(|| previous.and_then(|request| request.query)),
            open,
            refresh,
        });
    });
    post(hwnd, EVENT_WINDOW_SEARCH, search_version() as isize);
}

unsafe fn queue_window_choice(hwnd: HWND, combo: HWND, first_match: bool) {
    if combo_updating() || search_composing() {
        return;
    }
    let index = SendMessageW(combo, CB_GETCURSEL, 0, 0);
    if index < 0 && !first_match {
        return;
    }
    let index = index.max(0) as usize;
    let choice = WINDOW_OPTIONS.with(|options| options.borrow().get(index).cloned());
    if choice.is_some() {
        WINDOW_CHOICE.with(|pending| *pending.borrow_mut() = choice);
        invalidate_window_search();
        post(hwnd, EVENT_WINDOW_CHOOSE, next_interaction() as isize);
    }
}

fn interaction_version() -> usize {
    INTERACTION_VERSION.with(Cell::get)
}

fn next_interaction() -> usize {
    INTERACTION_VERSION.with(|version| {
        let next = version.get().wrapping_add(1).max(1);
        version.set(next);
        next
    })
}

fn picker_version() -> usize {
    PICKER_VERSION.with(Cell::get)
}

fn next_picker_version() -> usize {
    PICKER_VERSION.with(|version| {
        let next = version.get().wrapping_add(1).max(1);
        version.set(next);
        next
    })
}

unsafe fn owns_picker_capture(hwnd: HWND) -> bool {
    // 标题栏移动和边框缩放也会捕获主窗，只有准星拖动可标记为选窗捕获
    PICKER_CAPTURE.with(|capture| capture.get() != 0 && capture.get() == picker_version())
        && GetCapture() == hwnd
}

unsafe fn release_picker_capture(hwnd: HWND) {
    let owned = owns_picker_capture(hwnd);
    // 先撤销选窗标记，避免释放产生的同步通知误取消下一次拖动
    PICKER_CAPTURE.with(|capture| capture.set(0));
    if owned {
        ReleaseCapture();
    }
}

struct DeferredEventFlush {
    hwnd: HWND,
    state: *const RefCell<App>,
}

struct LayoutScope(bool);

impl LayoutScope {
    fn new() -> Self {
        Self(LAYOUT_IN_PROGRESS.with(|active| active.replace(true)))
    }
}

impl Drop for LayoutScope {
    fn drop(&mut self) {
        LAYOUT_IN_PROGRESS.with(|active| active.set(self.0));
    }
}

impl Drop for DeferredEventFlush {
    fn drop(&mut self) {
        if self.state.is_null() {
            return;
        }
        unsafe {
            if (*self.state).try_borrow().is_err() {
                return;
            }
            let valid = IsWindow(self.hwnd) != 0
                && GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) == self.state as isize;
            DEFERRED_EVENTS.with(|events| {
                let mut events = events.borrow_mut();
                let mut remaining = VecDeque::new();
                while let Some((hwnd, event, parameter)) = events.pop_front() {
                    if hwnd != self.hwnd {
                        remaining.push_back((hwnd, event, parameter));
                    } else if valid {
                        post(hwnd, event, parameter);
                    }
                }
                *events = remaining;
            });
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

unsafe fn read_text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let length = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

fn pixel(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 + 48) / 96
}

unsafe fn main_window_active(hwnd: HWND) -> bool {
    IsWindowVisible(hwnd) != 0 && IsWindowEnabled(hwnd) != 0 && GetForegroundWindow() == hwnd
}

unsafe fn list_header_height(hwnd: HWND, dpi: u32) -> i32 {
    let list = GetDlgItem(hwnd, ID_CHANGED_LIST);
    if !list.is_null() {
        let header = SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND;
        let mut bounds: RECT = zeroed();
        if !header.is_null() && GetWindowRect(header, &mut bounds) != 0 {
            let height = bounds.bottom - bounds.top;
            if height > 0 {
                return height;
            }
        }
    }
    pixel(DEFAULT_HEADER_HEIGHT, dpi)
}

unsafe fn minimum_client_height(hwnd: HWND, dpi: u32) -> i32 {
    // 两个页面共用主界面最小尺寸，设置内容在中间区域滚动
    pixel(MIN_MAIN_BODY_HEIGHT, dpi) + list_header_height(hwnd, dpi)
}

fn settings_body_control(id: i32) -> bool {
    matches!(
        id,
        ID_BASIC_TITLE
            | ID_DEFAULT_CONTAINER
            | ID_CLOSE_TO_TRAY
            | ID_SHOW_ICONS
            | ID_PRIVACY
            | ID_LANGUAGE_LABEL
            | ID_LANGUAGE
            | ID_HOTKEY_TITLE
            | ID_HOTKEY_LABEL
            | ID_HOTKEY_INPUT
            | ID_HOTKEY_SAVE
            | ID_HOTKEY_ENABLED
            | ID_PRESETS_TITLE
            | ID_PRESETS_SAVE
    ) || (ID_PRESET_LABEL..ID_PRESET_LABEL + 5).contains(&id)
        || (ID_PRESET_INPUT..ID_PRESET_INPUT + 5).contains(&id)
}

fn control_caption(id: i32) -> Option<&'static str> {
    Some(match id {
        ID_PICK => tr("拖动选窗", "Pick a window"),
        ID_REFRESH => tr("刷新", "Refresh"),
        ID_TARGET => tr("未选择窗口", "No selection"),
        ID_TOPMOST => tr("置顶", "On top"),
        5 => tr("透明度", "Transparency"),
        ID_RESTORE => tr("恢复当前", "Restore current"),
        ID_RESTORE_ALL | ID_HOTKEY_LABEL => tr("恢复全部", "Restore all"),
        ID_SETTINGS | ID_SETTINGS_TITLE => tr("设置", "Settings"),
        ID_SETTINGS_BACK => tr("返回", "Back"),
        ID_BASIC_TITLE => tr("基础配置", "General"),
        ID_DEFAULT_CONTAINER => tr("默认启用独立容器", "Use containers by default"),
        ID_CLOSE_TO_TRAY => tr("关闭至托盘", "Close to tray"),
        ID_SHOW_ICONS => tr("展示图标", "Show icons"),
        ID_PRIVACY => tr("隐私模式", "Privacy mode"),
        ID_LANGUAGE_LABEL => tr("语言", "Language"),
        ID_HOTKEY_TITLE => tr("快捷键", "Shortcut"),
        ID_HOTKEY_ENABLED => tr("启用", "Enabled"),
        ID_HOTKEY_SAVE | ID_PRESETS_SAVE => tr("保存", "Save"),
        ID_PRESETS_TITLE => tr("预设挡位", "Presets"),
        ID_EXIT => tr("退出 Trans", "Exit Trans"),
        _ => return None,
    })
}

unsafe fn set_button_selected(hwnd: HWND, selected: bool) {
    let key = wide(BUTTON_SELECTED_PROP);
    if selected {
        SetPropW(hwnd, key.as_ptr(), 1usize as HANDLE);
    } else {
        RemovePropW(hwnd, key.as_ptr());
    }
    InvalidateRect(hwnd, null(), 0);
}

unsafe fn paint_chrome(hwnd: HWND) {
    let mut paint: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if !dc.is_null() {
        let mut rect: RECT = zeroed();
        GetClientRect(hwnd, &mut rect);
        if let Some(buffer) = ui_theme::PaintBuffer::new(dc, paint.rcPaint) {
            draw_chrome(hwnd, buffer.dc(), &rect);
            if !buffer.present(dc) {
                draw_chrome(hwnd, dc, &rect);
            }
        } else {
            draw_chrome(hwnd, dc, &rect);
        }
    }
    EndPaint(hwnd, &paint);
}

unsafe fn draw_chrome(hwnd: HWND, dc: HDC, rect: &RECT) {
    FillRect(dc, rect, GetStockObject(WHITE_BRUSH));
    let saved = SaveDC(dc);
    if saved == 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = rect.right * 96 / dpi as i32;
    let height = rect.bottom * 96 / dpi as i32;
    SelectObject(dc, GetStockObject(DC_PEN));
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    SetDCPenColor(dc, ui_theme::BORDER);
    MoveToEx(dc, pixel(12, dpi), pixel(height - 40, dpi), null_mut());
    LineTo(dc, pixel(width - 12, dpi), pixel(height - 40, dpi));
    if IsWindowVisible(GetDlgItem(hwnd, ID_SETTINGS_TITLE)) != 0 {
        MoveToEx(dc, pixel(12, dpi), pixel(48, dpi), null_mut());
        LineTo(dc, pixel(width - 12, dpi), pixel(48, dpi));
    } else {
        RoundRect(
            dc,
            pixel(12, dpi),
            pixel(215, dpi),
            pixel(width - 12, dpi),
            pixel(height - 45, dpi),
            pixel(8, dpi),
            pixel(8, dpi),
        );
    }
    RestoreDC(dc, saved);
}

fn same_window(left: &WindowInfo, right: &WindowInfo) -> bool {
    left.hwnd == right.hwnd
        && left.process_id == right.process_id
        && left.thread_id == right.thread_id
}

unsafe fn main_window_geometry(hwnd: HWND) -> Option<WindowGeometry> {
    // 最小化的屏幕坐标不是正常布局，保留此前正常状态的快照
    if IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 {
        return None;
    }
    let mut rect: RECT = zeroed();
    if GetWindowRect(hwnd, &mut rect) == 0 {
        return None;
    }
    let geometry = WindowGeometry {
        x: rect.left,
        y: rect.top,
        width: rect.right.checked_sub(rect.left)?,
        height: rect.bottom.checked_sub(rect.top)?,
    };
    geometry.is_valid().then_some(geometry)
}

unsafe fn fit_main_window_geometry(
    geometry: WindowGeometry,
    min_width: i32,
    min_height: i32,
) -> Option<WindowGeometry> {
    if !geometry.is_valid() {
        return None;
    }
    let rect = RECT {
        left: geometry.x,
        top: geometry.y,
        right: geometry.x + geometry.width,
        bottom: geometry.y + geometry.height,
    };
    let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = zeroed();
    info.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        return None;
    }
    let work = info.rcWork;
    let work_width = work.right.checked_sub(work.left)?.max(1);
    let work_height = work.bottom.checked_sub(work.top)?.max(1);
    let width = geometry.width.max(min_width).min(work_width);
    let height = geometry.height.max(min_height).min(work_height);
    // 有可操作标题栏时保持原坐标，允许原本就有的少量边缘越界
    let visible_width = width.min(64);
    let visible_caption = height.min(32);
    let caption_reachable = i64::from(geometry.x) + i64::from(width)
        >= i64::from(work.left) + i64::from(visible_width)
        && i64::from(geometry.x) <= i64::from(work.right) - i64::from(visible_width)
        && geometry.y >= work.top
        && i64::from(geometry.y) + i64::from(visible_caption) <= i64::from(work.bottom);
    let (x, y) = if caption_reachable && width == geometry.width && height == geometry.height {
        (geometry.x, geometry.y)
    } else {
        (
            geometry.x.clamp(work.left, work.right - width),
            geometry.y.clamp(work.top, work.bottom - height),
        )
    };
    Some(WindowGeometry {
        x,
        y,
        width,
        height,
    })
}

fn screen_point(value: usize) -> POINT {
    POINT {
        x: value as u16 as i16 as i32,
        y: (value >> 16) as u16 as i16 as i32,
    }
}

unsafe fn in_transparency_controls(parent: HWND, point: POINT) -> bool {
    let value = GetDlgItem(parent, ID_VALUE);
    let restore = GetDlgItem(parent, ID_RESTORE_ALL);
    if value.is_null()
        || restore.is_null()
        || IsWindowVisible(value) == 0
        || IsWindowVisible(restore) == 0
    {
        return false;
    }
    let mut value_rect: RECT = zeroed();
    let mut restore_rect: RECT = zeroed();
    let mut client: RECT = zeroed();
    if GetWindowRect(value, &mut value_rect) == 0
        || GetWindowRect(restore, &mut restore_rect) == 0
        || GetClientRect(parent, &mut client) == 0
    {
        return false;
    }
    let mut top = POINT {
        x: value_rect.left,
        y: value_rect.top,
    };
    let mut bottom = POINT {
        x: restore_rect.right,
        y: restore_rect.bottom,
    };
    if ScreenToClient(parent, &mut top) == 0 || ScreenToClient(parent, &mut bottom) == 0 {
        return false;
    }
    // 以实际控件边界保护整个透明度操作区，留白点击也保留当前选择
    let padding = pixel(4, GetDpiForWindow(parent).max(96));
    point.x >= client.left
        && point.x < client.right
        && point.y >= top.y - padding
        && point.y < bottom.y + padding
}

enum PopupRequest {
    Window {
        window: WindowInfo,
        taskbar_hidden: bool,
        container_mode: bool,
        container_available: bool,
        topmost: Option<bool>,
        point: POINT,
    },
    Tray {
        point: POINT,
    },
}

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}

unsafe fn show_popup(hwnd: HWND, request: &PopupRequest) -> Result<u32, String> {
    let (items, point) = match request {
        PopupRequest::Window {
            point,
            taskbar_hidden,
            container_mode,
            topmost,
            ..
        } => (
            vec![
                if *container_mode {
                    (
                        MENU_DISABLE_CONTAINER,
                        tr("取消独立容器", "Disable container"),
                    )
                } else {
                    (
                        MENU_ENABLE_CONTAINER,
                        tr("启用独立容器", "Enable container"),
                    )
                },
                if *topmost == Some(true) {
                    (MENU_CANCEL_TOPMOST, tr("取消置顶", "Cancel always on top"))
                } else {
                    (MENU_TOPMOST_WINDOW, tr("置顶窗口", "Keep window on top"))
                },
                if *taskbar_hidden {
                    (MENU_SHOW_TASKBAR, tr("取消隐藏", "Unhide"))
                } else {
                    (MENU_HIDE_TASKBAR, tr("隐藏", "Hide"))
                },
                (MENU_RESTORE_WINDOW, tr("移除", "Remove")),
            ],
            *point,
        ),
        PopupRequest::Tray { point } => (
            vec![
                (MENU_OPEN, tr("打开 Trans", "Open Trans")),
                (MENU_SETTINGS, tr("设置", "Settings")),
                (MENU_RESTORE_ALL, tr("恢复全部窗口", "Restore all windows")),
                (0, ""),
                (MENU_EXIT, tr("退出", "Exit")),
            ],
            *point,
        ),
    };
    if let PopupRequest::Window {
        container_available,
        topmost,
        ..
    } = request
    {
        let mut disabled = Vec::new();
        if !*container_available {
            disabled.push(MENU_ENABLE_CONTAINER);
        }
        if topmost.is_none() {
            disabled.extend([MENU_TOPMOST_WINDOW, MENU_CANCEL_TOPMOST]);
        }
        return compact_menu::show(hwnd, &items, point, &disabled);
    }
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return Err(format!(
            "{} {}",
            tr(
                "无法创建菜单，Windows 错误码",
                "Cannot create the menu; Windows error"
            ),
            GetLastError()
        ));
    }
    let _menu = PopupMenu(menu);
    for (id, label) in items {
        if AppendMenuW(
            menu,
            if id == 0 { MF_SEPARATOR } else { MF_STRING },
            id as usize,
            wide(label).as_ptr(),
        ) == 0
        {
            return Err(format!(
                "{} {}",
                tr(
                    "无法创建菜单项，Windows 错误码",
                    "Cannot create the menu item; Windows error"
                ),
                GetLastError()
            ));
        }
    }
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenuEx(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        hwnd,
        null(),
    );
    // 托盘菜单结束后投递空消息，让系统正确关闭菜单焦点
    PostMessageW(hwnd, WM_NULL, 0, 0);
    Ok(command as u32)
}

unsafe fn set_list_cell(list: HWND, row: i32, column: i32, value: &str) {
    let mut text = wide(value);
    let item = LVITEMW {
        iSubItem: column,
        pszText: text.as_mut_ptr(),
        ..zeroed()
    };
    SendMessageW(
        list,
        LVM_SETITEMTEXTW,
        row as usize,
        &item as *const LVITEMW as isize,
    );
}

fn minimum_list_columns(width: i32, dpi: u32) -> [i32; 3] {
    let minimums = [pixel(72, dpi), pixel(52, dpi), pixel(54, dpi)];
    let total: i32 = minimums.iter().sum();
    if total <= width {
        minimums
    } else {
        let percent = (width as i64 * minimums[1] as i64 / total as i64) as i32;
        let process = (width as i64 * minimums[2] as i64 / total as i64) as i32;
        [width - percent - process, percent, process]
    }
}

fn proportional_list_columns(width: i32, dpi: u32, weights: [u32; 3]) -> [i32; 3] {
    let total = weights
        .iter()
        .map(|value| *value as i64)
        .sum::<i64>()
        .max(1);
    let percent = (width as i64 * weights[1] as i64 / total) as i32;
    let process = (width as i64 * weights[2] as i64 / total) as i32;
    let mut widths = [width - percent - process, percent, process];
    let minimums = minimum_list_columns(width, dpi);
    for index in 0..3 {
        for donor in 0..3 {
            if index != donor && widths[index] < minimums[index] {
                let amount =
                    (minimums[index] - widths[index]).min((widths[donor] - minimums[donor]).max(0));
                widths[index] += amount;
                widths[donor] -= amount;
            }
        }
    }
    widths
}

unsafe fn queue_list_column_resize(list: HWND, index: usize, width: i32) {
    if index >= 2 || LIST_COLUMNS_SIZING.with(Cell::get) {
        return;
    }
    let previous = LIST_COLUMN_REQUEST.with(|request| request.replace(Some((index, width))));
    if previous.is_none() && PostMessageW(list, LIST_COLUMN_RESIZE, 0, 0) == 0 {
        LIST_COLUMN_REQUEST.with(|request| request.set(None));
    }
}

unsafe fn header_column_resize(lparam: LPARAM) -> Option<(usize, i32)> {
    let header = &*(lparam as *const NMHDR);
    let (index, mask, width) =
        if matches!(header.code, HDN_TRACKA | HDN_ENDTRACKA | HDN_ITEMCHANGINGA) {
            let notice = &*(lparam as *const NMHEADERA);
            let item = notice.pitem.as_ref()?;
            (notice.iItem, item.mask, item.cxy)
        } else {
            let notice = &*(lparam as *const NMHEADERW);
            let item = notice.pitem.as_ref()?;
            (notice.iItem, item.mask, item.cxy)
        };
    (index >= 0 && mask & HDI_WIDTH != 0).then_some((index as usize, width))
}

unsafe fn resize_list_column(list: HWND, index: usize, requested: i32) {
    fit_list_columns(list);
    let mut widths = [0; 3];
    for (index, width) in widths.iter_mut().enumerate() {
        *width = SendMessageW(list, LVM_GETCOLUMNWIDTH, index, 0) as i32;
    }
    let total: i32 = widths.iter().sum();
    if index >= 2 || total <= 0 {
        return;
    }
    let minimums = minimum_list_columns(total, GetDpiForWindow(list).max(96));
    let pair_width = widths[index] + widths[index + 1];
    if pair_width < minimums[index] + minimums[index + 1] {
        return;
    }
    widths[index] = requested.clamp(minimums[index], pair_width - minimums[index + 1]);
    widths[index + 1] = pair_width - widths[index];
    LIST_COLUMN_WEIGHTS.with(|weights| weights.set(widths.map(|width| width as u32)));
    fit_list_columns(list);
}

unsafe fn fit_list_columns(list: HWND) {
    let header = SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND;
    if header.is_null()
        || SendMessageW(header, HDM_GETITEMCOUNT, 0, 0) != 3
        || LIST_COLUMNS_SIZING.with(|sizing| sizing.replace(true))
    {
        return;
    }
    // 列宽和滚动条变化会同步触发 WM_SIZE，避免重入调整
    for _ in 0..3 {
        let mut bounds: RECT = zeroed();
        if GetClientRect(list, &mut bounds) == 0 {
            break;
        }
        let width = bounds.right - bounds.left;
        if width <= 0 {
            break;
        }
        let widths = proportional_list_columns(
            width,
            GetDpiForWindow(list).max(96),
            LIST_COLUMN_WEIGHTS.with(Cell::get),
        );
        // 先缩窄再扩宽，避免逐列更新时临时超过可用宽度
        for shrinking in [true, false] {
            for (index, target) in widths.iter().copied().enumerate() {
                let current = SendMessageW(list, LVM_GETCOLUMNWIDTH, index, 0) as i32;
                if current != target && (current > target) == shrinking {
                    SendMessageW(list, LVM_SETCOLUMNWIDTH, index, target as isize);
                }
            }
        }
        let offset = GetScrollPos(list, SB_HORZ);
        if offset != 0 {
            SendMessageW(list, LVM_SCROLL, (-offset) as usize, 0);
        }
        if GetWindowLongPtrW(list, GWL_STYLE) as u32 & WS_HSCROLL != 0 {
            ShowScrollBar(list, SB_HORZ, 0);
        }
        let mut updated: RECT = zeroed();
        GetClientRect(list, &mut updated);
        if updated.right - updated.left == width {
            break;
        }
    }
    LIST_COLUMNS_SIZING.with(|sizing| sizing.set(false));
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    SetWindowTextW(hwnd, wide(value).as_ptr());
}

unsafe fn post(hwnd: HWND, event: usize, parameter: isize) {
    PostMessageW(hwnd, APP_EVENT, event, parameter);
}

unsafe fn queue_layout(hwnd: HWND) {
    if !LAYOUT_PENDING.with(|pending| pending.replace(true))
        && PostMessageW(hwnd, APP_EVENT, EVENT_LAYOUT, 0) == 0
    {
        LAYOUT_PENDING.with(|pending| pending.set(false));
    }
}

unsafe fn layout_window(hwnd: HWND, state: *const RefCell<App>) {
    if !state.is_null() {
        if let Ok(mut app) = (*state).try_borrow_mut() {
            if !app.exiting {
                app.layout();
            }
            return;
        }
    }
    queue_layout(hwnd);
}

unsafe fn position_controls(positions: &[(HWND, [i32; 4])]) -> bool {
    if positions.is_empty() {
        return true;
    }
    // 禁止移动中间态重绘及旧像素拷贝，完成布局后统一刷新
    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS;
    let mut batch = BeginDeferWindowPos(positions.len() as i32);
    if !batch.is_null() {
        for &(hwnd, [x, y, width, height]) in positions {
            batch = DeferWindowPos(batch, hwnd, null_mut(), x, y, width, height, flags);
            if batch.is_null() {
                break;
            }
        }
        if !batch.is_null() && EndDeferWindowPos(batch) != 0 {
            return true;
        }
    }
    // 批量操作失败时保留相同的无重绘策略，逐个补齐布局
    let mut complete = true;
    for &(hwnd, [x, y, width, height]) in positions {
        complete &= SetWindowPos(hwnd, null_mut(), x, y, width, height, flags) != 0;
    }
    complete
}

unsafe fn set_extended_style(hwnd: HWND, style: isize) -> Result<(), String> {
    SetLastError(0);
    if SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style) == 0 && GetLastError() != 0 {
        return Err(format!(
            "{} {}",
            tr(
                "无法修改 Trans 窗口样式，Windows 错误码",
                "Cannot change the Trans window style; Windows error"
            ),
            GetLastError()
        ));
    }
    Ok(())
}

unsafe fn apply_self_transparency(hwnd: HWND, percent: u8, previous: u8) -> Result<(), String> {
    let original_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let layered = WS_EX_LAYERED as isize;
    let style = if percent == 0 {
        original_style & !layered
    } else {
        original_style | layered
    };
    if style != original_style {
        set_extended_style(hwnd, style)?;
    }
    if percent != 0 {
        let alpha = ((100 - percent as u32) * 255 + 50) / 100;
        if SetLayeredWindowAttributes(hwnd, 0, alpha as u8, LWA_ALPHA) == 0 {
            let code = GetLastError();
            let rollback = set_extended_style(hwnd, original_style);
            let alpha_restored = original_style & layered == 0
                || SetLayeredWindowAttributes(
                    hwnd,
                    0,
                    (((100 - previous as u32) * 255 + 50) / 100) as u8,
                    LWA_ALPHA,
                ) != 0;
            return Err(format!(
                "{} {code}{}",
                tr(
                    "无法调整 Trans 自身透明度，Windows 错误码",
                    "Cannot adjust Trans transparency; Windows error"
                ),
                if rollback.is_err() || !alpha_restored {
                    tr(
                        "；请重新打开 Trans 恢复界面",
                        "; reopen Trans to restore the interface",
                    )
                } else {
                    ""
                },
            ));
        }
    }
    RedrawWindow(
        hwnd,
        null(),
        null_mut(),
        RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
    );
    Ok(())
}

pub fn show_startup_error(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide(tr("Trans · 无法启动", "Trans · Startup error")).as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

struct MutexHandle(HANDLE);

impl Drop for MutexHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mutex = CreateMutexW(null(), 0, wide("Local\\Trans.MainWindow.1").as_ptr());
        if mutex.is_null() {
            return Err(format!(
                "{} {}",
                tr(
                    "无法初始化应用，Windows 错误码",
                    "Cannot initialize the application; Windows error"
                ),
                GetLastError()
            ));
        }
        let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
        let _mutex = MutexHandle(mutex);
        if already_running {
            let existing = FindWindowW(wide(CLASS_NAME).as_ptr(), null());
            if !existing.is_null() {
                ShowWindow(existing, SW_RESTORE);
                SetForegroundWindow(existing);
                post(existing, EVENT_SHOW_MAIN, 0);
            }
            return Ok(());
        }

        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES | ICC_HOTKEY_CLASS,
        };
        if InitCommonControlsEx(&controls) == 0 {
            return Err(tr(
                "无法初始化 Windows 界面控件",
                "Cannot initialize Windows UI controls",
            )
            .to_owned());
        }
        // 先于窗口状态创建，确保退出时先释放窗口和绘图对象，再关闭 GDI+
        let _graphics_session = ui_theme::GdiPlusSession::new();
        let instance = GetModuleHandleW(null());
        let name = wide(CLASS_NAME);
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(instance, ICON_RESOURCE as *const u16),
            hIconSm: LoadImageW(
                instance,
                ICON_RESOURCE as *const u16,
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_SHARED,
            ) as HICON,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszClassName: name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(format!(
                "{} {}",
                tr(
                    "无法创建窗口类，Windows 错误码",
                    "Cannot create the window class; Windows error"
                ),
                GetLastError()
            ));
        }

        let state = Box::new(RefCell::new(App::new()));
        let dpi = GetDpiForSystem().max(96);
        let style = MAIN_STYLE;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: pixel(WIDTH, dpi),
            bottom: pixel(HEIGHT, dpi),
        };
        AdjustWindowRectExForDpi(&mut rect, style, 0, WS_EX_CONTROLPARENT, dpi);
        let saved_geometry = state.borrow().settings.main_window;
        let geometry = saved_geometry.and_then(|saved| fit_main_window_geometry(saved, 1, 1));
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            name.as_ptr(),
            wide("Trans").as_ptr(),
            style,
            geometry.map_or(CW_USEDEFAULT, |saved| saved.x),
            geometry.map_or(CW_USEDEFAULT, |saved| saved.y),
            geometry.map_or(rect.right - rect.left, |saved| saved.width),
            geometry.map_or(rect.bottom - rect.top, |saved| saved.height),
            null_mut(),
            null_mut(),
            instance,
            (&*state as *const RefCell<App>).cast(),
        );
        if hwnd.is_null() {
            return Err(state
                .borrow_mut()
                .initialization_error
                .take()
                .unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        tr(
                            "无法创建主窗口，Windows 错误码",
                            "Cannot create the main window; Windows error"
                        ),
                        GetLastError()
                    )
                }));
        }
        if let Some(saved) = geometry {
            // 创建在目标显示器后按其实际 DPI 校验最小尺寸，再显示窗口
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut minimum = RECT {
                right: pixel(MIN_WIDTH, dpi),
                bottom: minimum_client_height(hwnd, dpi),
                ..zeroed()
            };
            AdjustWindowRectExForDpi(&mut minimum, style, 0, WS_EX_CONTROLPARENT, dpi);
            if let Some(saved) = fit_main_window_geometry(
                saved,
                minimum.right - minimum.left,
                minimum.bottom - minimum.top,
            ) {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    saved.x,
                    saved.y,
                    saved.width,
                    saved.height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        state.borrow_mut().remember_main_window_geometry();
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        let mut message: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result == 0 {
                break;
            }
            if result == -1 {
                let code = GetLastError();
                DestroyWindow(hwnd);
                return Err(format!(
                    "{} {code}",
                    tr(
                        "消息循环异常，Windows 错误码",
                        "The message loop failed; Windows error"
                    )
                ));
            }
            if message.message == WM_KEYDOWN && message.wParam == VK_ESCAPE as usize {
                let (owned, composing, registered_escape) = {
                    let app = state.borrow();
                    let search = message.hwnd == app.combo_edit
                        || message.hwnd == app.combo_list
                        || message.hwnd == app.control(ID_WINDOWS);
                    (
                        message.hwnd == hwnd || IsChild(hwnd, message.hwnd) != 0 || search,
                        search && search_composing() && !app.picking,
                        app.hotkey
                            && GetFocus() != app.control(ID_HOTKEY_INPUT)
                            && app.settings.restore_hotkey.is_some_and(|hotkey| {
                                hotkey.key == u32::from(VK_ESCAPE) && hotkey.modifiers == 0
                            }),
                    )
                };
                let plain = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
                    .into_iter()
                    .all(|key| GetKeyState(key as i32) >= 0);
                if owned && main_window_active(hwnd) && plain && !composing {
                    // 同一次长按只触发一次，已注册的裸 Esc 统一由全局快捷键消息处理
                    if message.lParam & (1 << 30) == 0 && !registered_escape {
                        post(hwnd, EVENT_ESCAPE, 0);
                    }
                    continue;
                }
            }
            if message.message == WM_KEYDOWN && message.wParam == VK_RETURN as usize {
                let mut app = state.borrow_mut();
                if !search_composing()
                    && (message.hwnd == app.combo_edit
                        || message.hwnd == app.combo_list
                        || message.hwnd == app.control(ID_WINDOWS))
                {
                    let combo = app.control(ID_WINDOWS);
                    if SEARCH_REQUEST.with(|request| {
                        request
                            .borrow()
                            .as_ref()
                            .is_some_and(|request| request.query.is_some())
                    }) {
                        app.event(EVENT_WINDOW_SEARCH, search_version() as isize);
                    }
                    drop(app);
                    queue_window_choice(hwnd, combo, true);
                    continue;
                }
            }
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}

struct Control {
    hwnd: HWND,
    id: i32,
    bounds: Option<[i32; 4]>,
}

struct ControlTooltip {
    hwnd: HWND,
    _text: Vec<u16>,
}

impl ControlTooltip {
    unsafe fn new(parent: HWND, button: HWND, caption: &str) -> Option<Self> {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_NOPREFIX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            parent,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let mut text = wide(caption);
        let tool = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: parent,
            uId: button as usize,
            lpszText: text.as_mut_ptr(),
            ..zeroed()
        };
        if SendMessageW(hwnd, TTM_ADDTOOLW, 0, &tool as *const TTTOOLINFOW as isize) == 0 {
            DestroyWindow(hwnd);
            return None;
        }
        SendMessageW(hwnd, TTM_SETDELAYTIME, TTDT_INITIAL as usize, 500);
        Some(Self { hwnd, _text: text })
    }

    unsafe fn set_active(&self, active: bool) {
        if !active {
            SendMessageW(self.hwnd, TTM_POP, 0, 0);
        }
        SendMessageW(self.hwnd, TTM_ACTIVATE, active as usize, 0);
    }

    unsafe fn update_caption(&mut self, parent: HWND, button: HWND, caption: &str) {
        let mut text = wide(caption);
        let tool = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: parent,
            uId: button as usize,
            lpszText: text.as_mut_ptr(),
            ..zeroed()
        };
        SendMessageW(self.hwnd, TTM_POP, 0, 0);
        SendMessageW(
            self.hwnd,
            TTM_UPDATETIPTEXTW,
            0,
            &tool as *const TTTOOLINFOW as isize,
        );
        self._text = text;
    }
}

impl Drop for ControlTooltip {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

struct App {
    hwnd: HWND,
    controls: Vec<Control>,
    fonts: Vec<HFONT>,
    dpi: u32,
    layout_size: Option<(i32, i32, u32)>,
    windows: Vec<WindowInfo>,
    window_options: Vec<WindowInfo>,
    window_query: String,
    combo_edit: HWND,
    combo_list: HWND,
    changed_windows: Vec<ChangedWindowInfo>,
    changed_list_dirty: bool,
    pending_window_refresh: bool,
    pending_container_sync: bool,
    pending_changed_selection: Option<WindowInfo>,
    pending_changed_activation: Option<WindowInfo>,
    selected: Option<WindowInfo>,
    selected_valid: bool,
    controller: WindowController,
    window_events: Option<WindowEvents>,
    activity_timer_error: bool,
    picking: bool,
    picker_tooltip: Option<ControlTooltip>,
    refresh_tooltip: Option<ControlTooltip>,
    picker_overlay: Option<PickerOverlay>,
    picker_warning: Option<String>,
    hotkey: bool,
    privacy_names: Option<PrivacyNames>,
    privacy_error_reported: bool,
    icons: Option<IconList>,
    last_percent: u8,
    initialization_error: Option<String>,
    pending_error: Option<String>,
    settings: Settings,
    last_main_window: Option<WindowGeometry>,
    settings_page: bool,
    settings_panel: Option<SettingsPanel>,
    self_transparency: u8,
    startup_warning: Option<String>,
    tray: Option<TrayIcon>,
    taskbar_created: u32,
    pending_menu: Option<PopupRequest>,
    popup_open: bool,
    exit_pending: bool,
    exiting: bool,
    last_taskbar_error: Option<String>,
}

impl App {
    fn new() -> Self {
        let (settings, startup_warning) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        set_language(settings.language);
        Self {
            hwnd: null_mut(),
            controls: Vec::new(),
            fonts: Vec::new(),
            dpi: 0,
            layout_size: None,
            windows: Vec::new(),
            window_options: Vec::new(),
            window_query: String::new(),
            combo_edit: null_mut(),
            combo_list: null_mut(),
            changed_windows: Vec::new(),
            changed_list_dirty: true,
            pending_window_refresh: false,
            pending_container_sync: false,
            pending_changed_selection: None,
            pending_changed_activation: None,
            selected: None,
            selected_valid: false,
            controller: WindowController::new(),
            window_events: None,
            activity_timer_error: false,
            picking: false,
            picker_tooltip: None,
            refresh_tooltip: None,
            picker_overlay: None,
            picker_warning: None,
            hotkey: false,
            privacy_names: None,
            privacy_error_reported: false,
            icons: None,
            last_percent: 0,
            initialization_error: None,
            pending_error: None,
            last_main_window: settings.main_window,
            settings,
            settings_page: false,
            settings_panel: None,
            self_transparency: 0,
            startup_warning,
            tray: None,
            taskbar_created: 0,
            pending_menu: None,
            popup_open: false,
            exit_pending: false,
            exiting: false,
            last_taskbar_error: None,
        }
    }

    fn control(&self, id: i32) -> HWND {
        self.controls
            .iter()
            .find(|item| item.id == id)
            .map_or(null_mut(), |item| item.hwnd)
    }

    unsafe fn add(&mut self, class: &str, text: &str, id: i32, style: u32) -> Result<(), String> {
        let text = control_caption(id).unwrap_or(text);
        let parent = if settings_body_control(id) {
            self.settings_panel
                .as_ref()
                .ok_or(tr(
                    "设置滚动区域尚未初始化",
                    "The settings scroll area has not been initialized",
                ))?
                .hwnd()
        } else {
            self.hwnd
        };
        let hwnd = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            parent,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        if hwnd.is_null() {
            return Err(format!(
                "{} {id}{} {}",
                tr("无法创建界面控件", "Cannot create UI control"),
                tr("，Windows 错误码", "; Windows error"),
                GetLastError()
            ));
        }
        self.controls.push(Control {
            hwnd,
            id,
            bounds: None,
        });
        if settings_body_control(id) && !self.settings_panel.as_ref().unwrap().install_child(hwnd) {
            return Err(tr(
                "无法初始化设置滚动交互",
                "Cannot initialize settings scrolling",
            )
            .into());
        }
        if class == "BUTTON"
            && style & BS_TYPEMASK as u32 == BS_OWNERDRAW as u32
            && !ui_theme::install_button_hover(hwnd)
        {
            return Err(tr("无法初始化按钮样式", "Cannot initialize the button style").into());
        }
        Ok(())
    }

    unsafe fn initialize(&mut self, hwnd: HWND) -> Result<(), String> {
        self.hwnd = hwnd;
        crate::window_topmost::configure(hwnd, APP_EVENT, EVENT_TOPMOST_COMPLETED);
        self.add(
            "BUTTON",
            "置顶",
            ID_TOPMOST,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32 | BS_RIGHT as u32,
        )?;
        if !ui_theme::install_compact_checkbox(self.control(ID_TOPMOST)) {
            return Err(tr(
                "无法初始化置顶选项样式",
                "Cannot initialize the always-on-top option style",
            )
            .into());
        }
        self.add(
            "BUTTON",
            "拖动选窗",
            ID_PICK,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add(
            "COMBOBOX",
            "",
            ID_WINDOWS,
            WS_TABSTOP
                | WS_VSCROLL
                | CBS_DROPDOWN as u32
                | CBS_AUTOHSCROLL as u32
                | CBS_OWNERDRAWFIXED as u32
                | CBS_HASSTRINGS as u32,
        )?;
        self.add(
            "BUTTON",
            "刷新",
            ID_REFRESH,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add(
            "STATIC",
            "未选择窗口",
            ID_TARGET,
            SS_LEFT | SS_CENTERIMAGE | SS_ENDELLIPSIS | SS_NOPREFIX,
        )?;
        self.add("STATIC", "透明度", 5, SS_LEFT)?;
        self.add("STATIC", "0%", ID_VALUE, SS_RIGHT)?;
        self.add(
            "msctls_trackbar32",
            "",
            ID_SLIDER,
            WS_TABSTOP | TBS_NOTICKS | TBS_FIXEDLENGTH,
        )?;
        for (index, percent) in self.settings.preset_levels.into_iter().enumerate() {
            self.add(
                "BUTTON",
                &format!("{percent}%"),
                ID_PRESET + index as i32,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
        }
        self.add(
            "BUTTON",
            "恢复当前",
            ID_RESTORE,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add(
            "BUTTON",
            "恢复全部",
            ID_RESTORE_ALL,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add("STATIC", "窗口列表", ID_CHANGED_TITLE, SS_LEFT)?;
        self.add(
            "SysListView32",
            "",
            ID_CHANGED_LIST,
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_SHAREIMAGELISTS,
        )?;
        self.add(
            "BUTTON",
            "设置",
            ID_SETTINGS,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add("STATIC", "0%", ID_SELF_LABEL, SS_LEFT)?;
        self.add(
            "msctls_trackbar32",
            "",
            ID_SELF_SLIDER,
            WS_TABSTOP | TBS_NOTICKS,
        )?;
        self.add(
            "STATIC",
            "设置",
            ID_SETTINGS_TITLE,
            SS_LEFT | SS_CENTERIMAGE,
        )?;
        self.add(
            "BUTTON",
            "返回",
            ID_SETTINGS_BACK,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.settings_panel = Some(SettingsPanel::new(hwnd)?);
        self.add("STATIC", "基础配置", ID_BASIC_TITLE, SS_LEFT)?;
        self.add("STATIC", "", ID_LANGUAGE_LABEL, SS_LEFT | SS_CENTERIMAGE)?;
        self.add(
            "COMBOBOX",
            "",
            ID_LANGUAGE,
            WS_TABSTOP
                | CBS_DROPDOWNLIST as u32
                | CBS_OWNERDRAWFIXED as u32
                | CBS_HASSTRINGS as u32,
        )?;
        for name in ["中文", "English"] {
            if SendMessageW(
                self.control(ID_LANGUAGE),
                CB_ADDSTRING,
                0,
                wide(name).as_ptr() as isize,
            ) < 0
            {
                return Err(tr("无法初始化语言选项", "Cannot initialize language options").into());
            }
        }
        if !ui_theme::install_dropdown_theme(self.control(ID_LANGUAGE)) {
            return Err(tr(
                "无法初始化语言选择框样式",
                "Cannot initialize the language selector style",
            )
            .into());
        }
        self.add(
            "BUTTON",
            "默认启用独立容器",
            ID_DEFAULT_CONTAINER,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        )?;
        self.add(
            "BUTTON",
            "关闭至托盘",
            ID_CLOSE_TO_TRAY,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        )?;
        self.add(
            "BUTTON",
            "展示图标",
            ID_SHOW_ICONS,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        )?;
        self.add(
            "BUTTON",
            "隐私模式",
            ID_PRIVACY,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        )?;
        self.add("STATIC", "快捷键", ID_HOTKEY_TITLE, SS_LEFT)?;
        self.add(
            "BUTTON",
            "启用",
            ID_HOTKEY_ENABLED,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32 | BS_RIGHT as u32,
        )?;
        if !ui_theme::install_compact_checkbox(self.control(ID_HOTKEY_ENABLED)) {
            return Err(tr(
                "无法初始化快捷键启用选项样式",
                "Cannot initialize the shortcut enable option style",
            )
            .into());
        }
        self.add(
            "STATIC",
            "恢复全部",
            ID_HOTKEY_LABEL,
            SS_LEFT | SS_CENTERIMAGE,
        )?;
        self.add("msctls_hotkey32", "", ID_HOTKEY_INPUT, WS_TABSTOP)?;
        if !ui_theme::install_hotkey_theme(self.control(ID_HOTKEY_INPUT)) {
            return Err(tr(
                "无法初始化快捷键输入样式",
                "Cannot initialize the shortcut input style",
            )
            .into());
        }
        self.add(
            "BUTTON",
            "保存",
            ID_HOTKEY_SAVE,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add("STATIC", "预设挡位", ID_PRESETS_TITLE, SS_LEFT)?;
        for index in 0..5 {
            self.add(
                "STATIC",
                &format!("{}", index + 1),
                ID_PRESET_LABEL + index,
                SS_CENTER | SS_CENTERIMAGE,
            )?;
            self.add(
                "EDIT",
                "",
                ID_PRESET_INPUT + index,
                WS_TABSTOP | ES_NUMBER as u32 | ES_CENTER as u32 | ES_AUTOHSCROLL as u32,
            )?;
            let input = self.control(ID_PRESET_INPUT + index);
            SendMessageW(input, EM_SETLIMITTEXT, 3, 0);
            if !ui_theme::install_input_theme(input) {
                return Err(tr(
                    "无法初始化预设挡位输入样式",
                    "Cannot initialize the preset input style",
                )
                .into());
            }
        }
        self.add(
            "BUTTON",
            "保存",
            ID_PRESETS_SAVE,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        self.add(
            "BUTTON",
            "退出 Trans",
            ID_EXIT,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        let mut combo_info = COMBOBOXINFO {
            cbSize: size_of::<COMBOBOXINFO>() as u32,
            ..zeroed()
        };
        if GetComboBoxInfo(self.control(ID_WINDOWS), &mut combo_info) == 0 {
            return Err(tr(
                "无法初始化窗口搜索框",
                "Cannot initialize the window search box",
            )
            .into());
        }
        self.combo_edit = combo_info.hwndItem;
        self.combo_list = combo_info.hwndList;
        if !ui_theme::install_combo_theme(self.control(ID_WINDOWS), self.combo_edit) {
            return Err(tr(
                "无法初始化窗口搜索框样式",
                "Cannot initialize the window search box style",
            )
            .into());
        }
        if SetWindowSubclass(self.combo_edit, Some(search_edit_proc), 3, hwnd as usize) == 0 {
            return Err(tr(
                "无法初始化窗口搜索输入",
                "Cannot initialize window search input",
            )
            .into());
        }
        SendMessageW(self.control(ID_WINDOWS), CB_LIMITTEXT, 256, 0);
        for item in &self.controls {
            if matches!(
                item.id,
                5 | ID_TARGET | ID_VALUE | ID_CHANGED_TITLE | ID_SELF_LABEL
            ) {
                if SetWindowSubclass(item.hwnd, Some(background_proc), 4, hwnd as usize) == 0 {
                    return Err(tr(
                        "无法初始化界面选择状态",
                        "Cannot initialize the UI selection state",
                    )
                    .into());
                }
            }
        }
        let list = self.control(ID_CHANGED_LIST);
        if SetWindowSubclass(list, Some(list_theme_proc), 5, 0) == 0 {
            return Err(tr(
                "无法初始化窗口列表样式",
                "Cannot initialize the window list style",
            )
            .into());
        }
        if !crate::list_scrollbar::install(list) {
            return Err(tr(
                "无法初始化窗口列表滚动条样式",
                "Cannot initialize the window list scrollbar style",
            )
            .into());
        }
        SendMessageW(list, LVM_SETBKCOLOR, 0, ui_theme::SURFACE as isize);
        SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, ui_theme::SURFACE as isize);
        SendMessageW(list, LVM_SETTEXTCOLOR, 0, TEXT as isize);
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as usize,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize,
        );
        for (index, name) in [tr("窗口名称", "Window"), tr("透明度", "%"), "PID"]
            .iter()
            .enumerate()
        {
            let mut text = wide(name);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
                fmt: LVCFMT_LEFT as i32,
                cx: 100,
                pszText: text.as_mut_ptr(),
                ..zeroed()
            };
            if SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                index,
                &column as *const LVCOLUMNW as isize,
            ) == -1
            {
                return Err(tr(
                    "无法初始化已调整窗口列表",
                    "Cannot initialize the managed window list",
                )
                .to_owned());
            }
        }
        let header = SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND;
        if !header.is_null() {
            let style = GetWindowLongPtrW(header, GWL_STYLE);
            SetWindowLongPtrW(
                header,
                GWL_STYLE,
                (style & !(HDS_NOSIZING as isize)) | HDS_FULLDRAG as isize,
            );
        }
        if SetWindowSubclass(self.control(ID_PICK), Some(picker_proc), 1, hwnd as usize) == 0 {
            return Err(tr(
                "无法初始化拖动选窗，请重新打开应用",
                "Cannot initialize drag-to-select; reopen the application",
            )
            .to_owned());
        }
        self.picker_tooltip = ControlTooltip::new(
            hwnd,
            self.control(ID_PICK),
            tr("按住并拖动，选择窗口", "Hold and drag to pick a window"),
        );
        self.refresh_tooltip = ControlTooltip::new(
            hwnd,
            self.control(ID_REFRESH),
            tr("刷新窗口列表", "Refresh window list"),
        );
        SendMessageW(self.control(ID_SLIDER), TBM_SETRANGEMIN, 0, 0);
        SendMessageW(self.control(ID_SLIDER), TBM_SETRANGEMAX, 1, 100);
        SendMessageW(self.control(ID_SLIDER), TBM_SETPAGESIZE, 0, 5);
        SendMessageW(self.control(ID_SELF_SLIDER), TBM_SETRANGEMIN, 0, 0);
        SendMessageW(
            self.control(ID_SELF_SLIDER),
            TBM_SETRANGEMAX,
            1,
            MAX_SELF_TRANSPARENCY as isize,
        );
        SendMessageW(self.control(ID_SELF_SLIDER), TBM_SETPAGESIZE, 0, 5);
        SendMessageW(
            self.control(ID_CLOSE_TO_TRAY),
            BM_SETCHECK,
            if self.settings.close_to_tray {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            } as usize,
            0,
        );
        SendMessageW(
            self.control(ID_TOPMOST),
            BM_SETCHECK,
            BST_CHECKED as usize,
            0,
        );
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        for (id, checked) in [
            (ID_DEFAULT_CONTAINER, self.settings.default_container),
            (ID_SHOW_ICONS, self.settings.show_icons),
            (ID_PRIVACY, self.settings.privacy_mode),
        ] {
            self.set_checked(id, checked);
        }
        self.load_hotkey_controls();
        self.load_preset_controls();
        self.apply_language_text();
        if SetWindowSubclass(
            self.control(ID_HOTKEY_INPUT),
            Some(hotkey_proc),
            2,
            hwnd as usize,
        ) == 0
        {
            return Err(tr("无法初始化快捷键输入", "Cannot initialize shortcut input").into());
        }
        if let Err(error) = self.register_hotkey(self.settings.restore_hotkey) {
            self.report_error(&error);
        }
        self.layout();
        self.refresh();
        self.refresh_changed_list(true);
        self.update_page_visibility();
        self.set_self_transparency(self.settings.self_transparency, false);
        self.update_enabled();
        self.tray = Some(TrayIcon::new(hwnd));
        self.taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        if let Err(error) = self.ensure_tray() {
            self.report_error(&format!(
                "{error}\n{}",
                tr(
                    "主窗口会保持可见，请稍后重试",
                    "The main window will remain visible; try again later"
                )
            ));
        }
        if let Some(error) = self.startup_warning.take() {
            self.report_error(&format!(
                "{error}\n{}",
                tr(
                    "本次使用默认设置，可在设置页重新保存",
                    "Default settings are being used; save your preferences again in Settings"
                )
            ));
        }
        self.window_events = Some(WindowEvents::new(hwnd, TARGET_ACTIVITY_MESSAGE)?);
        self.sync_window_event_targets();
        Ok(())
    }

    unsafe fn layout(&mut self) {
        if self.controls.is_empty() || self.settings_panel.is_none() || IsIconic(self.hwnd) != 0 {
            return;
        }
        let dpi = GetDpiForWindow(self.hwnd).max(96);
        let mut rect: RECT = zeroed();
        if GetClientRect(self.hwnd, &mut rect) == 0 || rect.right <= 0 || rect.bottom <= 0 {
            return;
        }
        let size = (rect.right, rect.bottom, dpi);
        if self.layout_size == Some(size) {
            return;
        }
        let _layout_scope = LayoutScope::new();
        let dpi_changed = dpi != self.dpi;
        if dpi_changed {
            self.dpi = dpi;
            let mut fonts = Vec::new();
            for (height, weight, face) in [
                (12, FW_NORMAL, "Microsoft YaHei UI"),
                (20, FW_SEMIBOLD, "Microsoft YaHei UI"),
                (16, FW_NORMAL, "Segoe UI Symbol"),
                (9, FW_NORMAL, "Microsoft YaHei UI"),
                (11, FW_SEMIBOLD, "Microsoft YaHei UI"),
            ] {
                fonts.push(CreateFontW(
                    -pixel(height, dpi),
                    0,
                    0,
                    0,
                    weight as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_TT_ONLY_PRECIS as u32,
                    0,
                    CLEARTYPE_QUALITY as u32,
                    (DEFAULT_PITCH | FF_DONTCARE) as u32,
                    wide(face).as_ptr(),
                ));
            }
            for item in &self.controls {
                let font = match item.id {
                    ID_VALUE | ID_TARGET | ID_SETTINGS_TITLE => fonts[1],
                    ID_SETTINGS => fonts[2],
                    ID_SELF_LABEL => fonts[3],
                    5 | ID_CHANGED_TITLE | ID_BASIC_TITLE | ID_HOTKEY_TITLE | ID_PRESETS_TITLE => {
                        fonts[4]
                    }
                    _ => fonts[0],
                };
                let font = if font.is_null() {
                    GetStockObject(DEFAULT_GUI_FONT) as HFONT
                } else {
                    font
                };
                SendMessageW(item.hwnd, WM_SETFONT, font as usize, 0);
            }
            let tooltip_font = if fonts[0].is_null() {
                GetStockObject(DEFAULT_GUI_FONT) as HFONT
            } else {
                fonts[0]
            };
            for tooltip in [self.picker_tooltip.as_ref(), self.refresh_tooltip.as_ref()]
                .into_iter()
                .flatten()
            {
                SendMessageW(tooltip.hwnd, WM_SETFONT, tooltip_font as usize, 0);
            }
            let header = SendMessageW(self.control(ID_CHANGED_LIST), LVM_GETHEADER, 0, 0) as HWND;
            if !header.is_null() {
                let font = if fonts[4].is_null() {
                    GetStockObject(DEFAULT_GUI_FONT)
                } else {
                    fonts[4]
                };
                SendMessageW(header, WM_SETFONT, font as usize, 0);
            }
            for font in self.fonts.drain(..) {
                if !font.is_null() {
                    DeleteObject(font);
                }
            }
            self.fonts = fonts;
            self.changed_list_dirty = true;
            SendMessageW(
                self.control(ID_SLIDER),
                TBM_SETTHUMBLENGTH,
                pixel(32, dpi) as usize,
                0,
            );
            ui_theme::update_combo_metrics(self.control(ID_WINDOWS), dpi);
            ui_theme::update_combo_metrics(self.control(ID_LANGUAGE), dpi);
            ui_theme::update_hotkey_metrics(self.control(ID_HOTKEY_INPUT));
            for index in 0..5 {
                ui_theme::update_input_metrics(self.control(ID_PRESET_INPUT + index));
            }
        }
        let width = (rect.right * 96 / dpi as i32).max(MIN_WIDTH);
        let height = (rect.bottom * 96 / dpi as i32).max(MIN_MAIN_BODY_HEIGHT);
        let panel = self.settings_panel.as_ref().unwrap();
        let panel_positioned = panel.set_bounds(
            [
                pixel(12, dpi),
                pixel(52, dpi),
                pixel(width - 24, dpi),
                pixel(height - 96, dpi),
            ],
            dpi,
        );
        let body_width = panel.client_width() * 96 / dpi as i32;
        let scroll_offset = panel.scroll_offset();
        let english = self.settings.language == Language::English;
        let topmost_width = if english { 72 } else { 44 };
        let hotkey_label_width = if english { 78 } else { 56 };
        let hotkey_input_left = hotkey_label_width + 18;
        let hotkey_enabled_width = if english { 88 } else { 64 };
        let preset_save_width = 52;
        let preset_save_left = body_width - preset_save_width;
        // 按钮边框向内绘制，启用文字按实际可见边缘对齐并统一使用物理像素
        let settings_action_right =
            pixel(preset_save_left, dpi) + pixel(preset_save_width, dpi) - pixel(1, dpi) - 1;
        let header_only =
            !self.settings_page && rect.bottom <= minimum_client_height(self.hwnd, dpi);
        let list_x = 13;
        let list_y = 216;
        let list_width = width - 26;
        let list_height = height - list_y - 46;
        let restore_width = (width - 30) / 2;
        let mut positions = vec![
            (ID_PICK, 12, 8, 32, 32),
            (ID_WINDOWS, 52, 8, width - 104, 280),
            (ID_REFRESH, width - 44, 8, 32, 32),
            (ID_TARGET, 12, 42, width - topmost_width - 48, 32),
            (
                ID_TOPMOST,
                width - topmost_width - 12,
                46,
                topmost_width,
                24,
            ),
            (5, 12, 80, width - 100, 18),
            (ID_VALUE, width - 80, 72, 68, 28),
            (ID_SLIDER, 8, 100, width - 16, 32),
            (ID_RESTORE, 12, 166, restore_width, 26),
            (
                ID_RESTORE_ALL,
                18 + restore_width,
                166,
                width - 30 - restore_width,
                26,
            ),
            (ID_SETTINGS, 12, height - 37, 32, 32),
            (ID_SELF_LABEL, width - 34, height - 28, 22, 14),
            (ID_SELF_SLIDER, width - 122, height - 34, 88, 26),
            (ID_SETTINGS_TITLE, 12, 8, width - 64, 32),
            (ID_SETTINGS_BACK, width - 44, 8, 32, 32),
            (ID_BASIC_TITLE, 0, 8, body_width, 20),
            (ID_LANGUAGE_LABEL, 12, 54, 64, 20),
            (ID_LANGUAGE, 80, 48, body_width - 92, 120),
            (ID_DEFAULT_CONTAINER, 12, 92, body_width - 24, 24),
            (ID_CLOSE_TO_TRAY, 12, 128, body_width - 24, 24),
            (ID_SHOW_ICONS, 12, 164, body_width - 24, 24),
            (ID_PRIVACY, 12, 200, body_width - 24, 24),
            (
                ID_HOTKEY_TITLE,
                0,
                256,
                body_width - hotkey_enabled_width - 12,
                20,
            ),
            (ID_HOTKEY_LABEL, 12, 302, hotkey_label_width, 20),
            (
                ID_HOTKEY_INPUT,
                hotkey_input_left,
                296,
                body_width - hotkey_input_left - 70,
                32,
            ),
            (ID_HOTKEY_SAVE, body_width - 62, 296, 50, 32),
            (
                ID_HOTKEY_ENABLED,
                body_width - hotkey_enabled_width,
                252,
                hotkey_enabled_width,
                24,
            ),
            (ID_PRESETS_TITLE, 0, 356, body_width - 88, 20),
            (
                ID_PRESETS_SAVE,
                preset_save_left,
                350,
                preset_save_width,
                28,
            ),
            (ID_EXIT, width - 98, height - 37, 86, 32),
            (ID_CHANGED_TITLE, 12, 196, width - 24, 18),
            (ID_CHANGED_LIST, list_x, list_y, list_width, list_height),
        ];
        let gap = 6;
        for index in 0..5 {
            let left = 12 + index * (width - 24 + gap) / 5;
            let right = 12 + (index + 1) * (width - 24 + gap) / 5 - gap;
            positions.push((ID_PRESET + index, left, 136, right - left, 24));
            let input_left = 12 + index * (body_width - 16) / 5;
            let input_right = 12 + (index + 1) * (body_width - 16) / 5 - 8;
            positions.push((
                ID_PRESET_LABEL + index,
                input_left,
                398,
                input_right - input_left,
                16,
            ));
            positions.push((
                ID_PRESET_INPUT + index,
                input_left,
                418,
                input_right - input_left,
                32,
            ));
        }
        let positions: Vec<_> = positions
            .into_iter()
            .filter_map(|(id, x, y, width, height)| {
                let control = self.controls.iter().find(|control| control.id == id)?;
                let mut bounds = [
                    pixel(x, dpi),
                    pixel(y, dpi),
                    pixel(width, dpi),
                    pixel(height, dpi),
                ];
                if settings_body_control(id) {
                    bounds[1] -= scroll_offset;
                }
                if id == ID_HOTKEY_ENABLED {
                    bounds[0] = settings_action_right - bounds[2];
                }
                if id == ID_CHANGED_LIST && header_only {
                    bounds[3] = list_header_height(self.hwnd, dpi);
                }
                // 缓存请求尺寸，避免原生下拉框将展开高度折算为可见高度后反复移动
                (control.bounds != Some(bounds)).then_some((control.hwnd, bounds))
            })
            .collect();
        // 两种父窗口分别批量布局，避免跨父窗口的 DeferWindowPos 批次失败
        let (body_positions, root_positions): (Vec<_>, Vec<_>) = positions
            .iter()
            .copied()
            .partition(|(hwnd, _)| GetParent(*hwnd) == panel.hwnd());
        let root_positioned = position_controls(&root_positions);
        let body_positioned = position_controls(&body_positions);
        if body_positioned {
            panel.commit_layout(scroll_offset);
        }
        if panel_positioned && root_positioned && body_positioned {
            for (hwnd, bounds) in positions {
                if let Some(control) = self
                    .controls
                    .iter_mut()
                    .find(|control| control.hwnd == hwnd)
                {
                    control.bounds = Some(bounds);
                }
            }
            self.layout_size = Some(size);
        } else {
            // 失败的批次可能已移动部分控件，下一次布局重新应用全部尺寸
            self.layout_size = None;
            for control in &mut self.controls {
                control.bounds = None;
            }
        }
        ui_theme::update_hotkey_metrics(self.control(ID_HOTKEY_INPUT));
        for index in 0..5 {
            ui_theme::update_input_metrics(self.control(ID_PRESET_INPUT + index));
        }
        fit_list_columns(self.control(ID_CHANGED_LIST));
        if dpi_changed {
            self.refresh_changed_list(false);
        }
        RedrawWindow(
            self.hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE | RDW_FRAME,
        );
    }

    unsafe fn report_error(&mut self, message: &str) {
        // 隐私模式下不显示后端错误里可能包含的原始窗口名称
        let message = if self.settings.privacy_mode {
            self.private_error(message)
        } else {
            message.to_owned()
        };
        if let Some(pending) = &mut self.pending_error {
            pending.push_str("\n\n");
            pending.push_str(&message);
        } else {
            self.pending_error = Some(message);
            post(self.hwnd, EVENT_SHOW_ERROR, 0);
        }
    }

    unsafe fn update_page_visibility(&self) {
        if let Some(tooltip) = &self.picker_tooltip {
            tooltip.set_active(!self.settings_page && !self.picking);
        }
        if let Some(tooltip) = &self.refresh_tooltip {
            tooltip.set_active(!self.settings_page && !self.picking);
        }
        for control in &self.controls {
            let visible = match control.id {
                ID_SETTINGS => true,
                ID_SELF_LABEL | ID_SELF_SLIDER => !self.settings_page,
                ID_SETTINGS_TITLE..=ID_HOTKEY_ENABLED => self.settings_page,
                ID_BASIC_TITLE | ID_PRESETS_TITLE | ID_PRESETS_SAVE | ID_LANGUAGE_LABEL
                | ID_LANGUAGE | ID_DEFAULT_CONTAINER => self.settings_page,
                id if (ID_PRESET_LABEL..ID_PRESET_LABEL + 5).contains(&id)
                    || (ID_PRESET_INPUT..ID_PRESET_INPUT + 5).contains(&id) =>
                {
                    self.settings_page
                }
                _ => !self.settings_page,
            };
            ShowWindow(control.hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
        if let Some(panel) = &self.settings_panel {
            panel.show(self.settings_page);
        }
    }

    unsafe fn show_page(&mut self, settings: bool) {
        self.cancel_picker();
        if settings {
            self.clear_current_selection();
        }
        if self.settings_page != settings {
            // 页面切换后重新计算可见内容和固定底栏
            self.layout_size = None;
        }
        self.settings_page = settings;
        if !settings {
            self.refresh_managed_windows();
        }
        self.layout();
        set_button_selected(self.control(ID_SETTINGS), settings);
        self.update_page_visibility();
        SetFocus(self.control(if settings { ID_LANGUAGE } else { ID_PICK }));
        RedrawWindow(
            self.hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE | RDW_FRAME,
        );
    }

    unsafe fn apply_language_text(&mut self) {
        for control in &self.controls {
            if control.id == ID_TARGET && self.selected.is_some() {
                continue;
            }
            if let Some(caption) = control_caption(control.id) {
                set_text(control.hwnd, caption);
            }
        }
        set_text(
            self.control(ID_CHANGED_TITLE),
            &format!(
                "{} · {}",
                tr("窗口列表", "Windows"),
                self.changed_windows.len()
            ),
        );
        let list = self.control(ID_CHANGED_LIST);
        for (index, name) in [tr("窗口名称", "Window"), tr("透明度", "%"), "PID"]
            .iter()
            .enumerate()
        {
            let mut text = wide(name);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT,
                pszText: text.as_mut_ptr(),
                ..zeroed()
            };
            SendMessageW(
                list,
                LVM_SETCOLUMNW,
                index,
                &column as *const LVCOLUMNW as isize,
            );
        }
        let pick = self.control(ID_PICK);
        if let Some(tooltip) = &mut self.picker_tooltip {
            tooltip.update_caption(
                self.hwnd,
                pick,
                tr("按住并拖动，选择窗口", "Hold and drag to pick a window"),
            );
        }
        let refresh = self.control(ID_REFRESH);
        if let Some(tooltip) = &mut self.refresh_tooltip {
            tooltip.update_caption(
                self.hwnd,
                refresh,
                tr("刷新窗口列表", "Refresh window list"),
            );
        }
        self.sync_language_selection();
        InvalidateRect(self.control(ID_HOTKEY_INPUT), null(), 0);
    }

    unsafe fn sync_language_selection(&self) {
        let index = match self.settings.language {
            Language::Chinese => 0,
            Language::English => 1,
        };
        SendMessageW(self.control(ID_LANGUAGE), CB_SETCURSEL, index, 0);
    }

    unsafe fn change_language(&mut self, language: Language) {
        if self.settings.language == language {
            self.sync_language_selection();
            return;
        }
        let mut settings = self.settings.clone();
        settings.language = language;
        settings.self_transparency = self.self_transparency;
        if let Err(error) = settings.save() {
            self.sync_language_selection();
            self.report_error(&error);
            return;
        }
        self.settings = settings;
        set_language(language);
        self.apply_language_text();
        for (index, row) in self.changed_windows.clone().into_iter().enumerate() {
            let title = self.display_name(&row.window, row.token);
            set_list_cell(self.control(ID_CHANGED_LIST), index as i32, 0, &title);
        }
        if let Some(window) = &self.selected {
            set_text(self.control(ID_TARGET), &display_title(window));
        }
        self.rebuild_window_options(false);
        self.layout_size = None;
        self.layout();
    }

    unsafe fn show_window(&mut self, settings: bool) {
        ShowWindow(self.hwnd, SW_RESTORE);
        SetForegroundWindow(self.hwnd);
        self.show_page(settings);
    }

    unsafe fn ensure_tray(&mut self) -> Result<(), String> {
        if self.taskbar_created == 0 {
            return Err(tr(
                "无法注册托盘恢复消息，请重新打开 Trans",
                "Cannot register the tray recovery message; reopen Trans",
            )
            .into());
        }
        let icon = self
            .tray
            .as_mut()
            .ok_or(tr("托盘尚未初始化", "The tray has not been initialized"))?;
        if !icon.is_added() {
            icon.add()?;
        }
        Ok(())
    }

    unsafe fn remember_main_window_geometry(&mut self) {
        if let Some(geometry) = main_window_geometry(self.hwnd) {
            self.last_main_window = Some(geometry);
        }
    }

    unsafe fn save_current_settings(&mut self) -> Result<(), String> {
        self.remember_main_window_geometry();
        let mut settings = self.settings.clone();
        settings.self_transparency = self.self_transparency;
        settings.main_window = self.last_main_window;
        settings.save()?;
        self.settings = settings;
        Ok(())
    }

    fn private_error(&self, message: &str) -> String {
        let mut titles: Vec<&str> = self
            .windows
            .iter()
            .chain(self.changed_windows.iter().map(|row| &row.window))
            .chain(self.selected.iter())
            .map(|window| window.title.as_str())
            .filter(|title| !title.is_empty())
            .collect();
        titles.sort_by_key(|title| std::cmp::Reverse(title.len()));
        titles.dedup();
        titles.into_iter().fold(message.to_owned(), |text, title| {
            text.replace(title, tr("[窗口]", "[window]"))
        })
    }

    unsafe fn display_name(&mut self, window: &WindowInfo, token: usize) -> String {
        if !self.settings.privacy_mode {
            return if window.title.trim().is_empty() {
                tr("无标题窗口", "Untitled window").into()
            } else {
                window.title.replace(['\r', '\n', '\t'], " ")
            };
        }
        let name = (|| {
            if self.privacy_names.is_none() {
                self.privacy_names = Some(PrivacyNames::new()?);
            }
            self.privacy_names.as_mut().unwrap().title(window, token)
        })();
        match name {
            Ok(name) => name,
            Err(error) => {
                if !self.privacy_error_reported {
                    self.privacy_error_reported = true;
                    self.report_error(&format!(
                        "{error}\n{}",
                        tr(
                            "无法生成隐私码的窗口显示为 ???????",
                            "Windows without an available private name are shown as ???????"
                        )
                    ));
                }
                "???????".into()
            }
        }
    }

    unsafe fn set_checked(&self, id: i32, checked: bool) {
        SendMessageW(
            self.control(id),
            BM_SETCHECK,
            if checked { BST_CHECKED } else { BST_UNCHECKED } as usize,
            0,
        );
    }

    unsafe fn checked(&self, id: i32) -> bool {
        SendMessageW(self.control(id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize
    }

    unsafe fn set_display_option(&mut self, id: i32) {
        let mut settings = self.settings.clone();
        match id {
            ID_SHOW_ICONS => settings.show_icons = self.checked(id),
            ID_PRIVACY => settings.privacy_mode = self.checked(id),
            _ => return,
        }
        settings.self_transparency = self.self_transparency;
        if let Err(error) = settings.save() {
            self.set_checked(
                id,
                if id == ID_SHOW_ICONS {
                    self.settings.show_icons
                } else {
                    self.settings.privacy_mode
                },
            );
            self.report_error(&error);
            return;
        }
        self.settings = settings;
        // 开启隐私时也脱敏已经排队、尚未展示的错误
        if let Some(error) = self.pending_error.take() {
            self.pending_error = Some(if self.settings.privacy_mode {
                self.private_error(&error)
            } else {
                error
            });
        }
        self.refresh_changed_list(true);
        self.refresh();
    }

    unsafe fn set_default_container(&mut self) {
        let mut settings = self.settings.clone();
        settings.default_container = self.checked(ID_DEFAULT_CONTAINER);
        settings.self_transparency = self.self_transparency;
        if let Err(error) = settings.save() {
            self.set_checked(ID_DEFAULT_CONTAINER, self.settings.default_container);
            self.report_error(&error);
            return;
        }
        self.settings = settings;
    }

    unsafe fn load_preset_controls(&self) {
        for (index, value) in self.settings.preset_levels.iter().enumerate() {
            set_text(
                self.control(ID_PRESET_INPUT + index as i32),
                &value.to_string(),
            );
        }
    }

    unsafe fn save_preset_levels(&mut self) {
        let mut levels = [0_u8; 5];
        for (index, level) in levels.iter_mut().enumerate() {
            let input = self.control(ID_PRESET_INPUT + index as i32);
            let text = read_text(input);
            let text = text.trim();
            let value = (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| text.parse::<u8>().ok())
                .flatten()
                .filter(|value| *value <= 100);
            let Some(value) = value else {
                self.report_error(&format!(
                    "{} {} {}",
                    tr("预设挡位", "Preset"),
                    index + 1,
                    tr(
                        "必须为 0 到 100 的整数",
                        "must be an integer between 0 and 100"
                    )
                ));
                SetFocus(input);
                SendMessageW(input, EM_SETSEL, 0, -1);
                return;
            };
            *level = value;
        }
        let mut settings = self.settings.clone();
        settings.preset_levels = levels;
        settings.self_transparency = self.self_transparency;
        if let Err(error) = settings.save() {
            self.report_error(&error);
            return;
        }
        self.settings = settings;
        self.load_preset_controls();
        for (index, value) in self.settings.preset_levels.iter().enumerate() {
            set_text(self.control(ID_PRESET + index as i32), &format!("{value}%"));
        }
        self.update_enabled();
    }

    unsafe fn load_hotkey_controls(&self) {
        let hotkey = self.settings.restore_hotkey;
        self.set_checked(ID_HOTKEY_ENABLED, hotkey.is_some());
        let value = hotkey.map(|key| key.to_control().unwrap_or(0)).unwrap_or(0);
        SendMessageW(
            self.control(ID_HOTKEY_INPUT),
            HKM_SETHOTKEY,
            value as usize,
            0,
        );
        self.enable_hotkey_controls();
    }

    unsafe fn enable_hotkey_controls(&self) {
        let enabled = self.checked(ID_HOTKEY_ENABLED);
        EnableWindow(self.control(ID_HOTKEY_INPUT), enabled as i32);
    }

    unsafe fn unregister_hotkey(&mut self) -> Result<(), String> {
        if self.hotkey {
            SetLastError(0);
            if UnregisterHotKey(self.hwnd, HOTKEY_RESTORE) == 0 {
                let code = GetLastError();
                if code != ERROR_HOTKEY_NOT_REGISTERED {
                    return Err(format!(
                        "{} {code}",
                        tr(
                            "无法停用当前快捷键，Windows 错误码",
                            "Cannot disable the current shortcut; Windows error"
                        )
                    ));
                }
            }
            self.hotkey = false;
        }
        Ok(())
    }

    unsafe fn register_hotkey(&mut self, hotkey: Option<Hotkey>) -> Result<(), String> {
        if self.hotkey || hotkey.is_none() {
            return Ok(());
        }
        let key = hotkey.unwrap();
        key.validate()?;
        if RegisterHotKey(
            self.hwnd,
            HOTKEY_RESTORE,
            key.modifiers | MOD_NOREPEAT,
            key.key,
        ) == 0
        {
            return Err(format!(
                "{} {key}{} {}{}",
                tr("无法启用快捷键", "Cannot enable shortcut"),
                tr(
                    "，可能已被占用（Windows 错误码",
                    "; it may already be in use (Windows error"
                ),
                GetLastError(),
                tr("）", ")")
            ));
        }
        self.hotkey = true;
        Ok(())
    }

    unsafe fn save_hotkey(&mut self) {
        let requested = if self.checked(ID_HOTKEY_ENABLED) {
            let value = SendMessageW(self.control(ID_HOTKEY_INPUT), HKM_GETHOTKEY, 0, 0) as u16;
            match Hotkey::from_control(value) {
                Ok(Some(key)) => Some(key),
                Ok(None) => {
                    self.report_error(tr(
                        "请按下要使用的快捷键，或取消启用后保存",
                        "Press the shortcut you want to use, or turn off Enabled and save",
                    ));
                    return;
                }
                Err(error) => {
                    self.report_error(&error);
                    return;
                }
            }
        } else {
            None
        };
        let previous = self.settings.restore_hotkey;
        if let Err(error) = self.unregister_hotkey() {
            self.report_error(&error);
            return;
        }
        let mut settings = self.settings.clone();
        settings.restore_hotkey = requested;
        settings.self_transparency = self.self_transparency;
        let saved = self
            .register_hotkey(requested)
            .and_then(|_| settings.save());
        match saved {
            Ok(()) => {
                self.settings = settings;
                self.load_hotkey_controls();
            }
            Err(error) => {
                let rollback = self
                    .unregister_hotkey()
                    .and_then(|_| self.register_hotkey(previous));
                self.load_hotkey_controls();
                self.report_error(&error);
                if let Err(error) = rollback {
                    self.report_error(&error);
                }
            }
        }
    }

    unsafe fn set_self_transparency(&mut self, percent: u8, save: bool) {
        let percent = percent.min(MAX_SELF_TRANSPARENCY);
        if percent != self.self_transparency {
            match apply_self_transparency(self.hwnd, percent, self.self_transparency) {
                Ok(()) => self.self_transparency = percent,
                Err(error) => self.report_error(&error),
            }
        }
        SendMessageW(
            self.control(ID_SELF_SLIDER),
            TBM_SETPOS,
            1,
            self.self_transparency as isize,
        );
        set_text(
            self.control(ID_SELF_LABEL),
            &format!("{}%", self.self_transparency),
        );
        InvalidateRect(self.control(ID_SELF_SLIDER), null(), 0);
        if save && self.settings.self_transparency != self.self_transparency {
            if let Err(error) = self.save_current_settings() {
                self.report_error(&format!(
                    "{error}\n{}",
                    tr("本次透明度已应用，重新启动后仍使用上次保存的值", "The current transparency was applied; restarting will use the previously saved value")
                ));
            }
        }
    }

    unsafe fn set_close_to_tray(&mut self) {
        let checked =
            SendMessageW(self.control(ID_CLOSE_TO_TRAY), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let mut settings = self.settings.clone();
        settings.close_to_tray = checked;
        settings.self_transparency = self.self_transparency;
        match settings.save() {
            Ok(()) => {
                self.settings = settings;
            }
            Err(error) => {
                SendMessageW(
                    self.control(ID_CLOSE_TO_TRAY),
                    BM_SETCHECK,
                    if self.settings.close_to_tray {
                        BST_CHECKED
                    } else {
                        BST_UNCHECKED
                    } as usize,
                    0,
                );
                self.report_error(&format!(
                    "{error}\n{}",
                    tr(
                        "关闭方式未更改，请稍后重试",
                        "The close behavior was not changed; try again later"
                    )
                ));
            }
        }
    }

    unsafe fn handle_escape(&mut self) {
        if self.exiting || !main_window_active(self.hwnd) {
            return;
        }
        if self.popup_open {
            EndMenu();
            return;
        }
        if self.picking || owns_picker_capture(self.hwnd) {
            self.cancel_picker();
            return;
        }
        if GetFocus() == self.combo_edit && search_composing() {
            return;
        }
        for id in [ID_WINDOWS, ID_LANGUAGE] {
            let combo = self.control(id);
            if IsWindowVisible(combo) != 0 && SendMessageW(combo, CB_GETDROPPEDSTATE, 0, 0) != 0 {
                if id == ID_WINDOWS {
                    // 取消尚未处理的展开请求，防止关闭下拉后被异步重新展开
                    queue_window_search(self.hwnd, false, false, None);
                }
                SendMessageW(combo, CB_SHOWDROPDOWN, 0, 0);
                return;
            }
        }
        let mut gui = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..zeroed()
        };
        if GetGUIThreadInfo(0, &mut gui) != 0
            && gui.flags
                & (GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE | GUI_INMOVESIZE)
                != 0
        {
            if gui.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE) != 0 {
                EndMenu();
            }
            return;
        }
        if self.settings_page {
            self.show_page(false);
        } else {
            self.close(false);
        }
    }

    unsafe fn close(&mut self, exit: bool) {
        self.exit_pending = false;
        if self.exiting {
            return;
        }
        self.cancel_picker();
        self.remember_main_window_geometry();
        if self.settings.self_transparency != self.self_transparency
            || self.settings.main_window != self.last_main_window
        {
            if let Err(error) = self.save_current_settings() {
                // 配置写入失败不能阻止用户通过明确的退出入口关闭应用
                if !exit && self.settings.close_to_tray {
                    self.show_window(self.settings_page);
                    self.report_error(&error);
                    return;
                }
            }
        }
        if !exit && self.settings.close_to_tray {
            match self.ensure_tray() {
                Ok(()) => {
                    if let Some(tooltip) = &self.picker_tooltip {
                        tooltip.set_active(false);
                    }
                    if let Some(tooltip) = &self.refresh_tooltip {
                        tooltip.set_active(false);
                    }
                    ShowWindow(self.hwnd, SW_HIDE);
                }
                Err(error) => {
                    self.show_window(self.settings_page);
                    self.report_error(&format!("{error}\n{}", tr("主窗口会保持可见，可在设置页选择退出", "The main window will remain visible; use Exit in Settings to close Trans")));
                }
            }
        } else if self.restore_all() {
            self.window_events = None;
            KillTimer(self.hwnd, TIMER_TARGET_ACTIVITY);
            self.exiting = true;
            post(self.hwnd, EVENT_DESTROY, 0);
        } else {
            self.exit_pending = crate::window_topmost::has_pending_requests();
            self.show_window(false);
        }
    }

    unsafe fn queue_menu(&mut self, request: PopupRequest) {
        if self.popup_open || self.pending_menu.is_some() || self.exiting {
            return;
        }
        for error in self.controller.finish_topmost_requests() {
            self.report_error(&error);
        }
        if self.exit_pending && !crate::window_topmost::has_pending_requests() {
            self.close(true);
            return;
        }
        self.pending_menu = Some(request);
        post(self.hwnd, EVENT_SHOW_MENU, 0);
    }

    unsafe fn queue_window_menu(&mut self, source: HWND, coordinates: isize) {
        if source != self.control(ID_CHANGED_LIST)
            || self.settings_page
            || self.picking
            || self.changed_list_dirty
        {
            return;
        }
        let list = self.control(ID_CHANGED_LIST);
        let mut point = screen_point(coordinates as usize);
        let index = if coordinates == -1 {
            let selected = SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
            let index = if selected < 0 {
                SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_FOCUSED as isize)
            } else {
                selected
            };
            if index < 0 {
                return;
            }
            let mut rect = RECT {
                left: LVIR_BOUNDS as i32,
                ..zeroed()
            };
            if SendMessageW(
                list,
                LVM_GETITEMRECT,
                index as usize,
                &mut rect as *mut RECT as isize,
            ) == 0
            {
                return;
            }
            point = POINT {
                x: rect.left + pixel(8, self.dpi),
                y: rect.bottom,
            };
            if ClientToScreen(list, &mut point) == 0 {
                return;
            }
            index
        } else {
            let mut client = point;
            if ScreenToClient(list, &mut client) == 0 {
                return;
            }
            let mut hit = LVHITTESTINFO {
                pt: client,
                ..zeroed()
            };
            SendMessageW(
                list,
                LVM_SUBITEMHITTEST,
                0,
                &mut hit as *mut LVHITTESTINFO as isize,
            )
        };
        if index < 0 {
            return;
        }
        if let Some(row) = self.changed_windows.get(index as usize) {
            let container_mode = self.controller.is_container_mode(&row.window);
            let container_available = container_mode
                || self
                    .controller
                    .can_enable_container(&row.window)
                    .unwrap_or(false);
            // 弹出菜单前保存完整窗口身份，刷新和行索引变化不能改变操作目标
            self.queue_menu(PopupRequest::Window {
                window: row.window.clone(),
                taskbar_hidden: self.controller.is_taskbar_hidden(&row.window),
                container_mode,
                container_available,
                topmost: self.controller.is_topmost(&row.window).ok(),
                point,
            });
        }
    }

    unsafe fn menu_command(&mut self, request: PopupRequest, command: u32) {
        if self.exiting {
            return;
        }
        match (request, command) {
            (PopupRequest::Window { window, .. }, MENU_RESTORE_WINDOW) => {
                let current = self
                    .selected
                    .as_ref()
                    .is_some_and(|selected| same_window(selected, &window));
                let restored = self.controller.restore(&window);
                if restored.is_ok()
                    && self
                        .pending_changed_selection
                        .as_ref()
                        .is_some_and(|pending| same_window(pending, &window))
                {
                    self.pending_changed_selection = None;
                }
                self.refresh_changed_list(false);
                match restored {
                    Ok(()) => {
                        if current {
                            self.select(Some(window), false);
                        }
                    }
                    Err(error) => self.report_error(&error),
                }
                self.update_enabled();
            }
            (PopupRequest::Window { window, .. }, command)
                if command == MENU_ENABLE_CONTAINER || command == MENU_DISABLE_CONTAINER =>
            {
                let selected = self.selected.clone();
                let enabled = command == MENU_ENABLE_CONTAINER;
                if let Err(error) = self.controller.set_container_mode(&window, enabled) {
                    self.report_error(&error);
                }
                self.refresh_changed_list(false);
                self.select(selected, false);
            }
            (PopupRequest::Window { window, .. }, command)
                if command == MENU_TOPMOST_WINDOW || command == MENU_CANCEL_TOPMOST =>
            {
                if let Err(error) = self
                    .controller
                    .set_topmost(&window, command == MENU_TOPMOST_WINDOW)
                {
                    self.report_error(&error);
                }
                self.refresh_changed_list(false);
                self.update_enabled();
            }
            (PopupRequest::Window { window, .. }, command)
                if command == MENU_HIDE_TASKBAR || command == MENU_SHOW_TASKBAR =>
            {
                let hidden = command == MENU_HIDE_TASKBAR;
                match self.controller.set_taskbar_hidden(&window, hidden) {
                    Ok(()) => {}
                    Err(error) => self.report_error(&error),
                }
                self.refresh_changed_list(false);
                self.update_enabled();
            }
            (PopupRequest::Tray { .. }, MENU_OPEN) => self.show_window(false),
            (PopupRequest::Tray { .. }, MENU_SETTINGS) => self.show_window(true),
            (PopupRequest::Tray { .. }, MENU_RESTORE_ALL) => {
                self.cancel_picker();
                if !self.restore_all() {
                    self.show_window(false);
                }
            }
            (PopupRequest::Tray { .. }, MENU_EXIT) => self.close(true),
            _ => {}
        }
    }

    unsafe fn update_enabled(&self) {
        let available = self.selected_valid && !self.picking;
        EnableWindow(self.control(ID_SLIDER), available as i32);
        for id in ID_PRESET..ID_PRESET + 5 {
            EnableWindow(self.control(id), available as i32);
        }
        let changed = self
            .selected
            .as_ref()
            .is_some_and(|window| self.controller.is_changed(window));
        EnableWindow(self.control(ID_RESTORE), (changed && !self.picking) as i32);
        EnableWindow(
            self.control(ID_RESTORE_ALL),
            (self.controller.changed_count() > 0) as i32,
        );
        EnableWindow(self.control(ID_WINDOWS), (!self.picking) as i32);
        EnableWindow(self.control(ID_REFRESH), (!self.picking) as i32);
        EnableWindow(self.control(ID_CHANGED_LIST), (!self.picking) as i32);
        EnableWindow(self.control(ID_SETTINGS), (!self.picking) as i32);
        EnableWindow(self.control(ID_SELF_SLIDER), (!self.picking) as i32);
        SendMessageW(self.control(ID_PICK), BM_SETSTATE, self.picking as usize, 0);
        if let Some(tooltip) = &self.picker_tooltip {
            tooltip.set_active(!self.settings_page && !self.picking);
        }
        if let Some(tooltip) = &self.refresh_tooltip {
            tooltip.set_active(!self.settings_page && !self.picking);
        }
        for (index, percent) in self.settings.preset_levels.iter().enumerate() {
            set_button_selected(
                self.control(ID_PRESET + index as i32),
                self.selected_valid && self.last_percent == *percent,
            );
        }
        InvalidateRect(self.control(ID_SLIDER), null(), 0);
        InvalidateRect(self.control(ID_SELF_SLIDER), null(), 0);
    }

    unsafe fn maintain_taskbar_state(&mut self) {
        let failures = self.controller.reapply_taskbar_hidden();
        self.report_taskbar_failures(failures);
    }

    unsafe fn report_taskbar_failures(&mut self, failures: Vec<String>) {
        if failures.is_empty() {
            self.last_taskbar_error = None;
        } else {
            let error = failures.join("\n");
            if self.last_taskbar_error.as_ref() != Some(&error) {
                self.report_error(&error);
                self.last_taskbar_error = Some(error);
            }
        }
    }

    unsafe fn sync_window_event_targets(&mut self) {
        if let Some(events) = &mut self.window_events {
            events.set_targets(self.controller.event_targets());
        }
        self.arm_activity_timer();
    }

    unsafe fn arm_activity_timer(&mut self) {
        KillTimer(self.hwnd, TIMER_TARGET_ACTIVITY);
        let delay = self
            .window_events
            .as_ref()
            .and_then(WindowEvents::next_delay_ms);
        if let Some(delay) = delay {
            if SetTimer(self.hwnd, TIMER_TARGET_ACTIVITY, delay, None) == 0 {
                if !self.activity_timer_error {
                    self.activity_timer_error = true;
                    self.report_error(tr(
                        "无法安排窗口透明度恢复检查",
                        "Could not schedule the window transparency recovery check",
                    ));
                }
            } else {
                self.activity_timer_error = false;
            }
        }
    }

    unsafe fn process_target_activity(&mut self) {
        let updates = self
            .window_events
            .as_mut()
            .map(WindowEvents::take_ready)
            .unwrap_or_default();
        for (hwnd, token) in updates.ready {
            for error in self.controller.maintain_transparency(hwnd, token) {
                self.report_error(&error);
            }
        }
        if !updates.taskbar.is_empty() {
            let mut taskbar_failures = Vec::new();
            for (hwnd, token) in updates.taskbar {
                taskbar_failures.extend(self.controller.maintain_taskbar_hidden(hwnd, token));
            }
            self.report_taskbar_failures(taskbar_failures);
        }
        if updates.refresh_list {
            self.refresh_managed_windows_with_sync(updates.has_window_events);
        } else {
            // 中间补检只重新安排下一次检查，不遍历全部窗口或重绘列表
            self.arm_activity_timer();
        }
    }

    unsafe fn refresh_managed_windows(&mut self) {
        self.refresh_managed_windows_with_sync(true);
    }

    unsafe fn refresh_managed_windows_with_sync(&mut self, sync_containers: bool) {
        self.pending_container_sync |= sync_containers;
        if self.picking {
            // 拖动期间合并通知，结束或取消时补齐列表更新
            self.pending_window_refresh = true;
            self.sync_window_event_targets();
            return;
        }
        self.pending_window_refresh = false;
        if std::mem::take(&mut self.pending_container_sync) {
            self.refresh_changed_list(false);
        } else {
            self.refresh_changed_list_display(false);
        }
    }

    unsafe fn sync_changed_selection(&self) {
        let list = self.control(ID_CHANGED_LIST);
        let clear = LVITEMW {
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..zeroed()
        };
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            usize::MAX,
            &clear as *const LVITEMW as isize,
        );
        let target = self
            .pending_changed_selection
            .as_ref()
            .or(self.selected.as_ref());
        if let Some(index) = target.and_then(|target| {
            self.changed_windows
                .iter()
                .position(|row| same_window(&row.window, target))
        }) {
            let selected = LVITEMW {
                stateMask: LVIS_SELECTED | LVIS_FOCUSED,
                state: LVIS_SELECTED | LVIS_FOCUSED,
                ..zeroed()
            };
            SendMessageW(
                list,
                LVM_SETITEMSTATE,
                index,
                &selected as *const LVITEMW as isize,
            );
        }
    }

    unsafe fn refresh_changed_list(&mut self, force: bool) {
        for error in self.controller.sync_containers() {
            self.report_error(&error);
        }
        self.refresh_changed_list_display(force);
    }

    unsafe fn refresh_changed_list_display(&mut self, force: bool) {
        let rows = self.controller.changed_windows();
        // 显示刷新不触发容器同步，普通模式补检结束时也不带动容器维护
        self.sync_window_event_targets();
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| self.controller.validate_selection(selected).is_err())
        {
            // 所有刷新入口都清理失效选择，不依赖关闭通知前的旧列表
            self.selected = None;
            self.selected_valid = false;
            self.controller.clear_selection();
            set_text(self.control(ID_TARGET), tr("未选择窗口", "No selection"));
            self.show_percent(0);
            self.sync_changed_selection();
            self.update_enabled();
        }
        let same_order = rows.len() == self.changed_windows.len()
            && rows.iter().zip(&self.changed_windows).all(|(left, right)| {
                same_window(&left.window, &right.window) && left.token == right.token
            });
        let unchanged = same_order
            && rows.iter().zip(&self.changed_windows).all(|(left, right)| {
                left.window.title == right.window.title
                    && left.transparency == right.transparency
                    && left.taskbar_hidden == right.taskbar_hidden
                    && left.container_mode == right.container_mode
                    && left.topmost == right.topmost
            });
        if !force && unchanged && !self.changed_list_dirty {
            return;
        }
        let mut rebuild = force || self.changed_list_dirty || !same_order;
        let list = self.control(ID_CHANGED_LIST);
        let top = SendMessageW(list, LVM_GETTOPINDEX, 0, 0).max(0) as usize;
        let top_window = self.changed_windows.get(top).map(|row| row.window.clone());
        if !self.settings.show_icons || force || self.changed_list_dirty {
            SendMessageW(list, LVM_SETIMAGELIST, LVSIL_SMALL as usize, 0);
            self.icons = None;
        }
        if self.settings.show_icons && self.icons.is_none() {
            match IconList::new(self.dpi.max(96)) {
                Ok(icons) => {
                    rebuild = true;
                    SendMessageW(
                        list,
                        LVM_SETIMAGELIST,
                        LVSIL_SMALL as usize,
                        icons.image_list(),
                    );
                    self.icons = Some(icons);
                }
                Err(error) => self.report_error(&error),
            }
        }
        self.changed_windows = rows;
        self.changed_list_dirty = false;
        ui_theme::set_window_badges(self.changed_windows.iter().map(|row| {
            (
                row.window.hwnd as usize,
                ui_theme::WindowBadges {
                    hidden: row.taskbar_hidden,
                    topmost: row.topmost,
                    container: row.container_mode,
                },
            )
        }));
        // 只暂停可见列表，WM_SETREDRAW(TRUE) 会使隐藏的设置页列表重新可见
        let suspend_redraw = GetWindowLongPtrW(list, GWL_STYLE) as u32 & WS_VISIBLE != 0;
        if suspend_redraw {
            SendMessageW(list, WM_SETREDRAW, 0, 0);
        }
        if rebuild {
            SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        }
        if let Some(icons) = &mut self.icons {
            icons.sync(
                self.changed_windows
                    .iter()
                    .map(|row| (&row.window, row.token)),
            );
        }
        for index in 0..self.changed_windows.len() {
            let row = self.changed_windows[index].clone();
            let title = self.display_name(&row.window, row.token);
            if rebuild {
                let mut text = wide(&title);
                let item = LVITEMW {
                    mask: LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE | LVIF_INDENT,
                    iItem: index as i32,
                    pszText: text.as_mut_ptr(),
                    lParam: row.window.hwnd as isize,
                    iImage: self.icons.as_mut().map_or(I_IMAGENONE, |icons| {
                        icons.icon_index(&row.window, row.token)
                    }),
                    iIndent: 0,
                    ..zeroed()
                };
                let inserted =
                    SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const LVITEMW as isize);
                if inserted != index as isize {
                    // 行插入失败时清空显示，避免视觉行与窗口身份映射错位
                    self.changed_list_dirty = true;
                    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
                    self.report_error(tr(
                        "无法更新窗口列表，请点击刷新重试",
                        "Cannot update the window list; click Refresh to try again",
                    ));
                    break;
                }
            } else {
                // 标题和透明度原位更新，保留当前选择与滚动位置
                set_list_cell(list, index as i32, 0, &title);
            }
            set_list_cell(list, index as i32, 1, &format!("{}%", row.transparency));
            set_list_cell(list, index as i32, 2, &row.window.process_id.to_string());
        }
        if rebuild && !self.changed_list_dirty {
            self.sync_changed_selection();
            if !self.changed_windows.is_empty() {
                let target_top = top_window
                    .as_ref()
                    .and_then(|window| {
                        self.changed_windows
                            .iter()
                            .position(|row| same_window(&row.window, window))
                    })
                    .unwrap_or_else(|| top.min(self.changed_windows.len() - 1));
                let mut bounds = RECT {
                    left: LVIR_BOUNDS as i32,
                    ..zeroed()
                };
                if SendMessageW(list, LVM_GETITEMRECT, 0, &mut bounds as *mut RECT as isize) != 0 {
                    let row_height = bounds.bottom - bounds.top;
                    let current_top = SendMessageW(list, LVM_GETTOPINDEX, 0, 0);
                    let distance = (target_top as isize - current_top) * row_height as isize;
                    if row_height > 0 && distance != 0 {
                        SendMessageW(list, LVM_SCROLL, 0, distance);
                    }
                }
            }
        }
        if suspend_redraw {
            SendMessageW(list, WM_SETREDRAW, 1, 0);
        }
        fit_list_columns(list);
        InvalidateRect(list, null(), 1);
        set_text(
            self.control(ID_CHANGED_TITLE),
            &format!(
                "{} · {}",
                tr("窗口列表", "Windows"),
                self.changed_windows.len()
            ),
        );
        if let Some(row) = self
            .selected
            .as_ref()
            .and_then(|selected| {
                self.changed_windows
                    .iter()
                    .find(|row| same_window(&row.window, selected))
            })
            .cloned()
        {
            if let Some(selected) = &mut self.selected {
                selected.title = row.window.title.clone();
            }
            let title = display_title(&row.window);
            set_text(self.control(ID_TARGET), &title);
            if !self.picking && GetCapture() != self.control(ID_SLIDER) {
                self.show_percent(row.transparency);
            }
        }
        self.update_enabled();
        if !same_order {
            self.rebuild_window_options(false);
        }
    }

    unsafe fn refresh(&mut self) {
        self.refresh_changed_list(false);
        if let Some(selected) = self.selected.clone() {
            let tracked = self
                .changed_windows
                .iter()
                .find(|row| same_window(&row.window, &selected))
                .map(|row| row.window.clone());
            let current = tracked
                .or_else(|| window_control::describe_window(selected.hwnd))
                .filter(|current| {
                    current.process_id == selected.process_id
                        && current.thread_id == selected.thread_id
                });
            self.select(current, false);
        }
        self.windows = window_control::enumerate_windows();
        self.rebuild_window_options(false);
        self.sync_changed_selection();
    }

    unsafe fn rebuild_window_options(&mut self, open: bool) {
        if search_composing() {
            return;
        }
        let options = filter_windows(
            &self.windows,
            &self.changed_windows,
            &self.window_query,
            GetCurrentProcessId(),
        );
        let combo = self.control(ID_WINDOWS);
        let dropped = SendMessageW(combo, CB_GETDROPPEDSTATE, 0, 0) != 0;
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(
            combo,
            CB_GETEDITSEL,
            &mut start as *mut u32 as usize,
            &mut end as *mut u32 as isize,
        );
        set_combo_updating(true);
        SendMessageW(combo, CB_RESETCONTENT, 0, 0);
        self.window_options.clear();
        for window in options {
            let label = format!("{}  ·  PID {}", display_title(&window), window.process_id);
            let inserted = SendMessageW(combo, CB_ADDSTRING, 0, wide(&label).as_ptr() as isize);
            if inserted != self.window_options.len() as isize {
                SendMessageW(combo, CB_RESETCONTENT, 0, 0);
                self.window_options.clear();
                self.report_error(tr(
                    "无法更新窗口候选项，请点击刷新重试",
                    "Cannot update the available windows; click Refresh to try again",
                ));
                break;
            }
            self.window_options.push(window);
        }
        SendMessageW(combo, CB_SETCURSEL, usize::MAX, 0);
        set_text(combo, &self.window_query);
        WINDOW_OPTIONS.with(|options| *options.borrow_mut() = self.window_options.clone());
        let focused = GetFocus() == self.combo_edit || GetFocus() == combo;
        if focused
            && (open || dropped)
            && !self.window_options.is_empty()
            && !self.settings_page
            && !self.picking
        {
            SendMessageW(combo, CB_SHOWDROPDOWN, 1, 0);
        }
        if focused {
            let length = self.window_query.encode_utf16().count() as u32;
            let range = start.min(length).min(u16::MAX as u32)
                | (end.min(length).min(u16::MAX as u32) << 16);
            SendMessageW(combo, CB_SETEDITSEL, 0, range as isize);
        }
        set_combo_updating(false);
    }

    unsafe fn clear_current_selection(&mut self) {
        next_interaction();
        invalidate_window_search();
        self.pending_changed_selection = None;
        self.pending_changed_activation = None;
        WINDOW_CHOICE.with(|pending| {
            pending.borrow_mut().take();
        });
        self.window_query.clear();
        set_combo_updating(true);
        SendMessageW(self.control(ID_WINDOWS), CB_SHOWDROPDOWN, 0, 0);
        set_combo_updating(false);
        self.select(None, false);
        self.rebuild_window_options(false);
    }

    unsafe fn select(&mut self, selected: Option<WindowInfo>, track: bool) {
        // 刷新与恢复不能重新标记失效的选择，只有用户主动选窗可建立新的身份
        self.selected = if track {
            selected
        } else {
            selected.filter(|window| self.controller.validate_selection(window).is_ok())
        };
        self.selected_valid = false;
        if let Some(window) = self.selected.clone() {
            let title = display_title(&window);
            set_text(self.control(ID_TARGET), &title);
            let newly_added = track && !self.controller.is_changed(&window);
            let mut watched = if track {
                self.controller
                    .watch_selection(&window)
                    .and_then(|_| self.controller.track(&window))
            } else {
                self.controller.validate_selection(&window)
            };
            if watched.is_ok() && newly_added && self.settings.default_container {
                // 仅首次登记时尝试，失败和手动取消后重新选中都不会自动重试
                let enabled = self
                    .controller
                    .can_enable_container(&window)
                    .and_then(|supported| {
                        if supported {
                            self.controller.set_container_mode(&window, true)
                        } else {
                            Ok(())
                        }
                    });
                if let Err(error) = enabled {
                    self.report_error(&format!(
                        "{}\n{error}",
                        tr(
                            "默认启用独立容器失败，已保留窗口列表项",
                            "Could not enable the default container; the window remains in the list"
                        )
                    ));
                }
                watched = self.controller.validate_selection(&window);
            }
            let transparency = self.controller.transparency_of(&window);
            match (watched, transparency) {
                (Ok(()), Ok(percent)) => {
                    self.selected_valid = true;
                    self.show_percent(percent);
                }
                (Err(message), Ok(percent)) => {
                    self.show_percent(percent);
                    self.report_error(&message);
                }
                (Err(message), _) | (_, Err(message)) => {
                    self.show_percent(0);
                    self.report_error(&message);
                }
            }
        } else {
            self.controller.clear_selection();
            set_text(self.control(ID_TARGET), tr("未选择窗口", "No selection"));
            self.show_percent(0);
        }
        self.refresh_changed_list(false);
        self.sync_changed_selection();
        self.update_enabled();
    }

    unsafe fn show_percent(&mut self, percent: u8) {
        self.last_percent = percent;
        SendMessageW(self.control(ID_SLIDER), TBM_SETPOS, 1, percent as isize);
        set_text(self.control(ID_VALUE), &format!("{percent}%"));
        for (index, preset) in self.settings.preset_levels.iter().enumerate() {
            set_button_selected(
                self.control(ID_PRESET + index as i32),
                self.selected_valid && percent == *preset,
            );
        }
        InvalidateRect(self.control(ID_SLIDER), null(), 0);
    }

    unsafe fn apply_percent(&mut self, percent: u8) {
        let Some(window) = self.selected.clone() else {
            return;
        };
        if self.picking || !self.selected_valid {
            return;
        }
        match self.controller.apply(&window, percent) {
            Ok(()) => {
                self.show_percent(percent);
            }
            Err(message) => {
                let previous = self
                    .controller
                    .transparency_of(&window)
                    .unwrap_or(self.last_percent);
                self.show_percent(previous);
                self.report_error(&message);
            }
        }
        self.refresh_changed_list(false);
        self.update_enabled();
    }

    unsafe fn restore_all(&mut self) -> bool {
        let failures = self.controller.restore_all();
        self.refresh_changed_list(false);
        if let Some(window) = self.selected.clone() {
            if let Ok(percent) = self.controller.transparency_of(&window) {
                self.show_percent(percent);
            }
        }
        self.update_enabled();
        if failures.is_empty() {
            true
        } else {
            if !crate::window_topmost::has_pending_requests() {
                self.report_error(&failures.join("\n\n"));
            }
            false
        }
    }

    unsafe fn cancel_picker(&mut self) {
        release_picker_capture(self.hwnd);
        next_picker_version();
        if let Some(overlay) = &mut self.picker_overlay {
            overlay.hide();
        }
        self.picker_warning = None;
        PICK_FINISH.with(|pending| {
            pending.borrow_mut().take();
        });
        if !self.picking {
            if self.pending_window_refresh {
                self.refresh_managed_windows_with_sync(false);
            }
            return;
        }
        self.picking = false;
        SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
        self.update_enabled();
        if self.pending_window_refresh {
            self.refresh_managed_windows_with_sync(false);
        }
    }

    unsafe fn event(&mut self, event: usize, parameter: isize) {
        if self.exiting {
            return;
        }
        // 完成通知即使被消息队列延迟，也可在下一次用户事件中收敛恢复状态
        if event != EVENT_TOPMOST_COMPLETED {
            for error in self.controller.finish_topmost_requests() {
                self.report_error(&error);
            }
            if self.exit_pending && !crate::window_topmost::has_pending_requests() {
                self.close(true);
                return;
            }
        }
        match event {
            EVENT_LAYOUT => {
                LAYOUT_PENDING.with(|pending| pending.set(false));
                self.layout();
            }
            EVENT_SETTINGS_SCROLL => {
                self.layout_size = None;
                self.layout();
            }
            EVENT_WINDOW_SEARCH
                if parameter as usize == search_version() && !search_composing() =>
            {
                if let Some(request) = SEARCH_REQUEST.with(|request| request.borrow_mut().take()) {
                    if let Some(query) = request.query {
                        self.window_query = query;
                    }
                    if request.refresh {
                        self.windows = window_control::enumerate_windows();
                    }
                    self.rebuild_window_options(request.open);
                }
            }
            EVENT_WINDOW_CHOOSE
                if parameter as usize == interaction_version()
                    && !self.picking
                    && !self.settings_page =>
            {
                if let Some(requested) = WINDOW_CHOICE.with(|pending| pending.borrow_mut().take()) {
                    invalidate_window_search();
                    let current = window_control::describe_window(requested.hwnd)
                        .filter(|current| same_window(current, &requested));
                    self.window_query.clear();
                    set_combo_updating(true);
                    SendMessageW(self.control(ID_WINDOWS), CB_SHOWDROPDOWN, 0, 0);
                    set_combo_updating(false);
                    self.select(current, true);
                    self.refresh();
                }
            }
            EVENT_CLEAR_SELECTION
                if parameter as usize == interaction_version()
                    && !self.picking
                    && !self.popup_open =>
            {
                self.clear_current_selection()
            }
            EVENT_DEACTIVATE if parameter as usize == interaction_version() => {
                let foreground = GetForegroundWindow();
                let mut process_id = 0;
                GetWindowThreadProcessId(foreground, &mut process_id);
                if !foreground.is_null() && process_id != GetCurrentProcessId() {
                    self.cancel_picker();
                    self.clear_current_selection();
                }
            }
            EVENT_HOTKEY_FOCUS => {
                if GetFocus() == self.control(ID_HOTKEY_INPUT) {
                    if let Err(error) = self.unregister_hotkey() {
                        self.report_error(&error);
                    }
                } else if let Err(error) = self.register_hotkey(self.settings.restore_hotkey) {
                    self.report_error(&error);
                }
            }
            EVENT_HOTKEY_RESTORE => {
                let value = parameter as u32;
                let modifiers = value & 0xffff;
                let key = value >> 16;
                if self.hotkey
                    && GetFocus() != self.control(ID_HOTKEY_INPUT)
                    && self
                        .settings
                        .restore_hotkey
                        .is_some_and(|saved| saved.key == key && saved.modifiers == modifiers)
                {
                    if key == u32::from(VK_ESCAPE)
                        && modifiers == 0
                        && main_window_active(self.hwnd)
                    {
                        self.handle_escape();
                    } else {
                        self.cancel_picker();
                        self.restore_all();
                    }
                }
            }
            EVENT_SHOW_MAIN => self.show_window(false),
            EVENT_TRAY_READD => {
                if let Some(tray) = &mut self.tray {
                    if let Err(error) = tray.add() {
                        self.show_window(self.settings_page);
                        self.report_error(&format!("{error}\n{}", tr("已重新显示主窗口，可稍后关闭到托盘", "The main window is visible again; you can close it to the tray later")));
                    }
                }
                self.maintain_taskbar_state();
            }
            EVENT_WINDOW_REFRESH => self.refresh_managed_windows(),
            EVENT_TARGET_ACTIVITY => self.process_target_activity(),
            EVENT_TOPMOST_COMPLETED => {
                for error in self.controller.finish_topmost_requests() {
                    self.report_error(&error);
                }
                self.refresh_changed_list_display(false);
                self.update_enabled();
                if self.exit_pending && !crate::window_topmost::has_pending_requests() {
                    self.close(true);
                }
            }
            EVENT_CHANGED_SELECT
                if parameter as usize == interaction_version()
                    && !self.picking
                    && !self.settings_page =>
            {
                if let Some(requested) = self.pending_changed_selection.take() {
                    self.refresh_changed_list(false);
                    let selected = self
                        .changed_windows
                        .iter()
                        .find(|row| same_window(&row.window, &requested))
                        .map(|row| row.window.clone());
                    if let Some(selected) = selected {
                        self.select(Some(selected), true);
                        self.refresh();
                    }
                }
            }
            EVENT_CHANGED_ACTIVATE if !self.picking && !self.settings_page => {
                if let Some(window) = self.pending_changed_activation.take() {
                    match self.controller.activate(&window) {
                        Ok(_) => {}
                        Err(error) => self.report_error(&error),
                    }
                    self.refresh_changed_list(false);
                }
            }
            EVENT_PICK_START
                if !self.settings_page
                    && parameter as usize == picker_version()
                    && owns_picker_capture(self.hwnd) =>
            {
                invalidate_window_search();
                self.pending_changed_selection = None;
                self.pending_changed_activation = None;
                WINDOW_CHOICE.with(|pending| {
                    pending.borrow_mut().take();
                });
                self.picker_warning = None;
                self.picking = true;
                if self.picker_overlay.is_none() {
                    match PickerOverlay::new(self.hwnd) {
                        Ok(overlay) => self.picker_overlay = Some(overlay),
                        Err(error) => self.picker_warning = Some(error),
                    }
                }
                SetFocus(self.hwnd);
                SetCursor(LoadCursorW(null_mut(), IDC_CROSS));
                self.update_enabled();
                post(self.hwnd, EVENT_PICK_MOVE, parameter);
            }
            EVENT_PICK_MOVE
                if self.picking
                    && parameter as usize == picker_version()
                    && owns_picker_capture(self.hwnd) =>
            {
                SetCursor(LoadCursorW(null_mut(), IDC_CROSS));
                let mut point: POINT = zeroed();
                let target = if GetCursorPos(&mut point) != 0 {
                    window_control::describe_window(GetAncestor(WindowFromPoint(point), GA_ROOT))
                } else {
                    None
                };
                if let Some(overlay) = &mut self.picker_overlay {
                    if let Some(window) = target {
                        if let Err(error) = overlay.show(&window) {
                            self.picker_warning.get_or_insert(error);
                        }
                    } else {
                        overlay.hide();
                    }
                }
            }
            EVENT_PICK_FINISH
                if self.picking
                    && parameter as usize == picker_version()
                    && owns_picker_capture(self.hwnd) =>
            {
                let target = PICK_FINISH
                    .with(|pending| pending.borrow_mut().take())
                    .filter(|(_, revision)| *revision == parameter as usize)
                    .and_then(|(target, _)| target)
                    .and_then(|requested| {
                        window_control::describe_window(requested.hwnd)
                            .filter(|current| same_window(current, &requested))
                    });
                if let Some(overlay) = &mut self.picker_overlay {
                    overlay.hide();
                }
                self.picking = false;
                release_picker_capture(self.hwnd);
                SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
                if let Some(window) = target {
                    self.select(Some(window), true);
                    self.refresh();
                } else {
                    self.update_enabled();
                }
                if self.pending_window_refresh {
                    self.refresh_managed_windows_with_sync(false);
                }
                if let Some(error) = self.picker_warning.take() {
                    self.report_error(&error);
                }
            }
            EVENT_PICK_CANCEL if parameter as usize == picker_version() => self.cancel_picker(),
            EVENT_SLIDER => self.apply_percent(parameter.clamp(0, 100) as u8),
            EVENT_SELF_SLIDER => self.set_self_transparency(
                (parameter as u32 & 0xff) as u8,
                parameter as u32 & 0x100 != 0,
            ),
            EVENT_COMMAND => {
                let id = parameter as u32 & 0xffff;
                match id as i32 {
                    ID_SETTINGS => self.show_page(!self.settings_page),
                    ID_SETTINGS_BACK => self.show_page(false),
                    ID_CLOSE_TO_TRAY => self.set_close_to_tray(),
                    ID_DEFAULT_CONTAINER => self.set_default_container(),
                    ID_SHOW_ICONS | ID_PRIVACY => self.set_display_option(id as i32),
                    ID_HOTKEY_ENABLED => self.enable_hotkey_controls(),
                    ID_HOTKEY_SAVE => self.save_hotkey(),
                    ID_PRESETS_SAVE => self.save_preset_levels(),
                    ID_EXIT => self.close(true),
                    ID_REFRESH => {
                        self.refresh();
                    }
                    ID_RESTORE => {
                        if let Some(window) = self.selected.clone() {
                            match self.controller.restore(&window) {
                                Ok(()) => {
                                    self.select(Some(window), false);
                                }
                                Err(message) => self.report_error(&message),
                            }
                            self.refresh_changed_list(false);
                            self.update_enabled();
                        }
                    }
                    ID_RESTORE_ALL => {
                        self.cancel_picker();
                        self.restore_all();
                    }
                    ID_TOPMOST => {
                        let checked = SendMessageW(self.control(ID_TOPMOST), BM_GETCHECK, 0, 0)
                            == BST_CHECKED as isize;
                        SetWindowPos(
                            self.hwnd,
                            if checked {
                                HWND_TOPMOST
                            } else {
                                HWND_NOTOPMOST
                            },
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                    id if (ID_PRESET..ID_PRESET + 5).contains(&id) => {
                        self.apply_percent(self.settings.preset_levels[(id - ID_PRESET) as usize])
                    }
                    _ => {}
                }
            }
            EVENT_LANGUAGE_CHANGE => match parameter {
                0 => self.change_language(Language::Chinese),
                1 => self.change_language(Language::English),
                _ => self.sync_language_selection(),
            },
            EVENT_ESCAPE => self.handle_escape(),
            EVENT_CLOSE => self.close(false),
            EVENT_EXIT => self.close(true),
            _ => {}
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            KillTimer(self.hwnd, TIMER_TARGET_ACTIVITY);
            self.window_events = None;
            self.tray = None;
            self.picker_overlay = None;
            self.picker_tooltip = None;
            self.refresh_tooltip = None;
            self.settings_panel = None;
            ui_theme::set_window_badges(std::iter::empty());
            PICK_FINISH.with(|pending| {
                pending.borrow_mut().take();
            });
            if IsWindow(self.control(ID_CHANGED_LIST)) != 0 {
                SendMessageW(
                    self.control(ID_CHANGED_LIST),
                    LVM_SETIMAGELIST,
                    LVSIL_SMALL as usize,
                    0,
                );
            }
            self.icons = None;
            if self.hotkey {
                UnregisterHotKey(self.hwnd, HOTKEY_RESTORE);
            }
            for font in self.fonts.drain(..) {
                if !font.is_null() {
                    DeleteObject(font);
                }
            }
        }
    }
}

unsafe extern "system" fn list_theme_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == LIST_COLUMN_RESIZE {
        if let Some((index, width)) = LIST_COLUMN_REQUEST.with(|request| request.take()) {
            resize_list_column(hwnd, index, width);
        }
        return 0;
    }
    if message == WM_NOTIFY && lparam != 0 {
        let header = &*(lparam as *const NMHDR);
        if header.hwndFrom == SendMessageW(hwnd, LVM_GETHEADER, 0, 0) as HWND {
            match header.code {
                NM_CUSTOMDRAW => {
                    return ui_theme::draw_header(&*(lparam as *const NMCUSTOMDRAW));
                }
                HDN_BEGINTRACKA | HDN_BEGINTRACKW => {
                    let index = (*(lparam as *const NMHEADERW)).iItem;
                    return (!(0..2).contains(&index)) as LRESULT;
                }
                HDN_TRACKA | HDN_TRACKW | HDN_ENDTRACKA | HDN_ENDTRACKW => {
                    if let Some((index, width)) = header_column_resize(lparam) {
                        queue_list_column_resize(hwnd, index, width);
                    }
                    return 0;
                }
                HDN_ITEMCHANGINGA | HDN_ITEMCHANGINGW if !LIST_COLUMNS_SIZING.with(Cell::get) => {
                    if let Some((index, width)) = header_column_resize(lparam) {
                        // 先拒绝单列默认改宽，退出原生通知后再联动修改相邻列
                        queue_list_column_resize(hwnd, index, width);
                        return 1;
                    }
                }
                HDN_DIVIDERDBLCLICKA | HDN_DIVIDERDBLCLICKW => {
                    return 1;
                }
                _ => {}
            }
        }
    }
    if message == WM_NCDESTROY {
        LIST_COLUMN_REQUEST.with(|request| request.set(None));
        RemoveWindowSubclass(hwnd, Some(list_theme_proc), 5);
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_SIZE | WM_DPICHANGED_AFTERPARENT | WM_SETFONT | LVM_SETIMAGELIST
    ) && !LAYOUT_IN_PROGRESS.with(Cell::get)
    {
        fit_list_columns(hwnd);
    }
    result
}

unsafe extern "system" fn search_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    parent: usize,
) -> LRESULT {
    let parent = parent as HWND;
    if (message == WM_KEYDOWN || message == WM_IME_KEYDOWN)
        && wparam == VK_ESCAPE as usize
        && !search_composing()
    {
        queue_window_search(parent, false, false, None);
    }
    if message == WM_IME_STARTCOMPOSITION {
        SEARCH_COMPOSING.with(|composing| composing.set(true));
        SEARCH_COMPOSITION_VERSION.with(|version| version.set(search_version()));
    }
    if message == WM_GETDLGCODE && lparam != 0 {
        let key = &*(lparam as *const MSG);
        if key.message == WM_KEYDOWN
            && (key.wParam == VK_RETURN as usize || key.wParam == VK_ESCAPE as usize)
            && search_composing()
        {
            return DefSubclassProc(hwnd, message, wparam, lparam) | DLGC_WANTMESSAGE as isize;
        }
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(search_edit_proc), 3);
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if message == WM_LBUTTONUP
        && !combo_updating()
        && !search_composing()
        && SendMessageW(GetParent(hwnd), CB_GETDROPPEDSTATE, 0, 0) == 0
    {
        // 原生输入框先完成点击和文本选区，再展开窗口候选
        queue_window_search(parent, true, true, None);
    }
    if message == WM_IME_ENDCOMPOSITION {
        SEARCH_COMPOSING.with(|composing| composing.set(false));
        if !combo_updating() {
            let valid = SEARCH_COMPOSITION_VERSION.with(Cell::get) == search_version();
            // 组合输入期间清空或取消后，不再回填旧查询或展开候选框
            queue_window_search(parent, valid, false, valid.then(|| read_text(hwnd)));
        }
    }
    result
}

unsafe extern "system" fn background_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_LBUTTONDOWN && GetCapture().is_null() {
        let parent = parent as HWND;
        let mut point = screen_point(lparam as usize);
        let preserve = ClientToScreen(hwnd, &mut point) != 0
            && ScreenToClient(parent, &mut point) != 0
            && in_transparency_controls(parent, point);
        if !preserve {
            post(parent, EVENT_CLEAR_SELECTION, next_interaction() as isize);
        }
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(background_proc), 4);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn hotkey_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_SETFOCUS || message == WM_KILLFOCUS {
        post(parent as HWND, EVENT_HOTKEY_FOCUS, 0);
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(hotkey_proc), 2);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn picker_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_LBUTTONDOWN {
        let parent = parent as HWND;
        next_interaction();
        let revision = next_picker_version();
        PICKER_CAPTURE.with(|capture| capture.set(revision));
        SetCapture(parent);
        if owns_picker_capture(parent) {
            post(parent, EVENT_PICK_START, revision as isize);
        } else {
            PICKER_CAPTURE.with(|capture| capture.set(0));
            post(parent, EVENT_PICK_CANCEL, revision as isize);
        }
        return 0;
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(picker_proc), 1);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 活动通知直接进入已有事件处理路径，减少一次排队并保留重入保护
    let (message, wparam, lparam) = match message {
        TARGET_ACTIVITY_MESSAGE => (APP_EVENT, EVENT_TARGET_ACTIVITY, 0),
        WM_TIMER if wparam == TIMER_TARGET_ACTIVITY => {
            KillTimer(hwnd, TIMER_TARGET_ACTIVITY);
            (APP_EVENT, EVENT_TARGET_ACTIVITY, 0)
        }
        _ => (message, wparam, lparam),
    };
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    let _deferred_flush = DeferredEventFlush { hwnd, state };
    if message == APP_EVENT && !state.is_null() && (*state).try_borrow_mut().is_err() {
        // COM 可能嵌套调度消息，状态释放后再投递，避免丢事件或立即重投形成忙循环
        DEFERRED_EVENTS.with(|events| events.borrow_mut().push_back((hwnd, wparam, lparam)));
        return 0;
    }
    match message {
        WM_NCCREATE => {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        WM_DESTROY => {
            if !state.is_null() {
                if let Ok(mut app) = (*state).try_borrow_mut() {
                    app.window_events = None;
                }
            }
            KillTimer(hwnd, TIMER_TARGET_ACTIVITY);
            PostQuitMessage(0);
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        WM_CLOSE => {
            post(hwnd, EVENT_CLOSE, 0);
            return 0;
        }
        WM_QUERYENDSESSION => {
            // 查询时尚未确认结束会话，取消关机不能清除已管理的窗口状态
            return 1;
        }
        WM_SYSCOMMAND if wparam as u32 & 0xfff0 == SC_MAXIMIZE => {
            return 0;
        }
        WM_PAINT => {
            paint_chrome(hwnd);
            return 0;
        }
        WM_ERASEBKGND => {
            // WM_PAINT 已完整填底，避免窗口缩放时先擦白再显示界面
            return 1;
        }
        WM_MEASUREITEM if lparam != 0 => {
            let item = &mut *(lparam as *mut MEASUREITEMSTRUCT);
            if compact_menu::measure_item(item) {
                return 1;
            }
            if item.CtlType == ODT_COMBOBOX && matches!(item.CtlID as i32, ID_WINDOWS | ID_LANGUAGE)
            {
                item.itemHeight = pixel(26, GetDpiForWindow(hwnd).max(96)) as u32;
                return 1;
            }
        }
        WM_DRAWITEM if lparam != 0 => {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            if compact_menu::draw_item(item) {
                return 1;
            }
            if item.CtlType == ODT_COMBOBOX && matches!(item.CtlID as i32, ID_WINDOWS | ID_LANGUAGE)
            {
                ui_theme::draw_combo_item(item);
                return 1;
            }
            if item.CtlType == ODT_BUTTON {
                let style = match item.CtlID as i32 {
                    ID_PICK => ButtonStyle::Picker,
                    ID_REFRESH => ButtonStyle::Refresh,
                    ID_SETTINGS => ButtonStyle::Icon,
                    ID_SETTINGS_BACK => ButtonStyle::Back,
                    ID_EXIT => ButtonStyle::Primary,
                    _ => ButtonStyle::Secondary,
                };
                let selected =
                    !GetPropW(item.hwndItem, wide(BUTTON_SELECTED_PROP).as_ptr()).is_null();
                ui_theme::draw_button(item, style, selected);
                return 1;
            }
        }
        WM_COMMAND => {
            let id = (wparam & 0xffff) as i32;
            let notification = ((wparam >> 16) & 0xffff) as u32;
            if id == ID_WINDOWS {
                if !combo_updating() && !state.is_null() {
                    match notification {
                        CBN_EDITCHANGE if !search_composing() => {
                            queue_window_search(hwnd, true, false, Some(read_text(lparam as HWND)));
                        }
                        CBN_SELENDOK => queue_window_choice(hwnd, lparam as HWND, false),
                        CBN_DROPDOWN => queue_window_search(hwnd, false, true, None),
                        CBN_CLOSEUP => queue_window_search(hwnd, false, false, None),
                        _ => {}
                    }
                }
                return 0;
            }
            if id == ID_LANGUAGE {
                if notification == CBN_SELENDOK && !state.is_null() {
                    let index = SendMessageW(lparam as HWND, CB_GETCURSEL, 0, 0);
                    post(hwnd, EVENT_LANGUAGE_CHANGE, index);
                }
                return 0;
            }
            post(hwnd, EVENT_COMMAND, wparam as isize);
            return 0;
        }
        WM_HSCROLL if lparam != 0 => {
            let percent = SendMessageW(lparam as HWND, TBM_GETPOS, 0, 0);
            match GetDlgCtrlID(lparam as HWND) {
                ID_SLIDER => post(hwnd, EVENT_SLIDER, percent),
                ID_SELF_SLIDER => {
                    // 拖动时实时预览，松开或键盘调整结束后才写入配置
                    let save = (wparam as u32 & 0xffff) != TB_THUMBTRACK;
                    let parameter = percent.clamp(0, MAX_SELF_TRANSPARENCY as isize)
                        | if save { 0x100 } else { 0 };
                    post(hwnd, EVENT_SELF_SLIDER, parameter);
                }
                _ => {}
            }
            return 0;
        }
        WM_NOTIFY if lparam != 0 => {
            let header = &*(lparam as *const NMHDR);
            if header.code == NM_CUSTOMDRAW {
                match header.idFrom as i32 {
                    ID_SLIDER | ID_SELF_SLIDER => {
                        return ui_theme::draw_trackbar(
                            &*(lparam as *const NMCUSTOMDRAW),
                            header.idFrom == ID_SELF_SLIDER as usize,
                        );
                    }
                    ID_CHANGED_LIST => {
                        return ui_theme::draw_list(&mut *(lparam as *mut NMLVCUSTOMDRAW));
                    }
                    _ => {}
                }
            }
            if header.idFrom == ID_CHANGED_LIST as usize && header.code == LVN_ITEMCHANGED {
                let notification = &*(lparam as *const NMLISTVIEW);
                if notification.iItem >= 0
                    && notification.uChanged & LVIF_STATE != 0
                    && notification.uNewState & LVIS_SELECTED != 0
                    && notification.uOldState & LVIS_SELECTED == 0
                {
                    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
                    if !pointer.is_null() {
                        // 内部刷新持有状态借用，忽略由程序更新产生的同步选择通知
                        if let Ok(mut app) = (*pointer).try_borrow_mut() {
                            if !app.picking && !app.settings_page && !app.changed_list_dirty {
                                app.pending_changed_selection = app
                                    .changed_windows
                                    .get(notification.iItem as usize)
                                    .map(|row| row.window.clone());
                                if app.pending_changed_selection.is_some() {
                                    post(hwnd, EVENT_CHANGED_SELECT, next_interaction() as isize);
                                }
                            }
                        }
                    }
                }
            }
            if header.idFrom == ID_CHANGED_LIST as usize && header.code == NM_CLICK {
                let notification = &*(lparam as *const NMITEMACTIVATE);
                let mut hit = LVHITTESTINFO {
                    pt: notification.ptAction,
                    ..zeroed()
                };
                let index = SendMessageW(
                    header.hwndFrom,
                    LVM_SUBITEMHITTEST,
                    0,
                    &mut hit as *mut LVHITTESTINFO as isize,
                );
                if index < 0 && GetCapture().is_null() {
                    post(hwnd, EVENT_CLEAR_SELECTION, next_interaction() as isize);
                }
            }
            if header.idFrom == ID_CHANGED_LIST as usize && header.code == NM_RCLICK {
                let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
                if !pointer.is_null() {
                    if let Ok(mut app) = (*pointer).try_borrow_mut() {
                        app.queue_window_menu(header.hwndFrom, GetMessagePos() as isize);
                    }
                }
            }
            if header.idFrom == ID_CHANGED_LIST as usize && header.code == NM_DBLCLK {
                let notification = &*(lparam as *const NMITEMACTIVATE);
                if notification.iItem >= 0 {
                    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
                    if !pointer.is_null() {
                        if let Ok(mut app) = (*pointer).try_borrow_mut() {
                            if !app.picking && !app.settings_page && !app.changed_list_dirty {
                                // 双击时保存窗口身份，异步处理不能重新按行号查找
                                app.pending_changed_activation = app
                                    .changed_windows
                                    .get(notification.iItem as usize)
                                    .map(|row| row.window.clone());
                                if app.pending_changed_activation.is_some() {
                                    post(hwnd, EVENT_CHANGED_ACTIVATE, 0);
                                }
                            }
                        }
                    }
                }
            }
            return 0;
        }
        WM_MOUSEMOVE if owns_picker_capture(hwnd) => {
            post(hwnd, EVENT_PICK_MOVE, picker_version() as isize);
            return 0;
        }
        WM_LBUTTONDOWN if GetCapture().is_null() => {
            if !in_transparency_controls(hwnd, screen_point(lparam as usize)) {
                post(hwnd, EVENT_CLEAR_SELECTION, next_interaction() as isize);
            }
            return 0;
        }
        WM_ACTIVATEAPP => {
            if wparam == 0 {
                post(hwnd, EVENT_DEACTIVATE, interaction_version() as isize);
            } else {
                post(hwnd, EVENT_WINDOW_REFRESH, 0);
            }
        }
        WM_LBUTTONUP if owns_picker_capture(hwnd) => {
            // 使用这条鼠标消息的释放点，避免排队期间光标移动改变选择
            let position = GetMessagePos();
            let point = POINT {
                x: position as u16 as i16 as i32,
                y: (position >> 16) as u16 as i16 as i32,
            };
            let target =
                window_control::describe_window(GetAncestor(WindowFromPoint(point), GA_ROOT));
            let revision = picker_version();
            PICK_FINISH.with(|pending| *pending.borrow_mut() = Some((target, revision)));
            post(hwnd, EVENT_PICK_FINISH, revision as isize);
            return 0;
        }
        WM_CAPTURECHANGED => {
            let revision = PICKER_CAPTURE.with(|capture| capture.replace(0));
            if revision != 0 {
                post(hwnd, EVENT_PICK_CANCEL, revision as isize);
                return 0;
            }
        }
        WM_CANCELMODE if owns_picker_capture(hwnd) => {
            post(hwnd, EVENT_PICK_CANCEL, picker_version() as isize);
            return 0;
        }
        WM_HOTKEY if wparam == HOTKEY_RESTORE as usize => {
            post(hwnd, EVENT_HOTKEY_RESTORE, lparam);
            return 0;
        }
        WM_SIZE => {
            if wparam != SIZE_MINIMIZED as usize {
                layout_window(hwnd, state);
            }
            return 0;
        }
        WM_EXITSIZEMOVE => {
            layout_window(hwnd, state);
            return 0;
        }
        crate::settings_panel::SCROLL_CHANGED => {
            post(hwnd, EVENT_SETTINGS_SCROLL, 0);
            return 0;
        }
        WM_DPICHANGED => {
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout_window(hwnd, state);
            return 0;
        }
        WM_GETMINMAXINFO => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: pixel(MIN_WIDTH, dpi),
                bottom: minimum_client_height(hwnd, dpi),
            };
            AdjustWindowRectExForDpi(&mut rect, MAIN_STYLE, 0, WS_EX_CONTROLPARENT, dpi);
            let info = &mut *(lparam as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT {
                x: rect.right - rect.left,
                y: rect.bottom - rect.top,
            };
            return 0;
        }
        WM_CTLCOLOREDIT
            if GetDlgCtrlID(lparam as HWND) == ID_HOTKEY_INPUT
                || (ID_PRESET_INPUT..ID_PRESET_INPUT + 5)
                    .contains(&GetDlgCtrlID(lparam as HWND)) =>
        {
            let hdc = wparam as HDC;
            SetTextColor(
                hdc,
                if IsWindowEnabled(lparam as HWND) == 0 {
                    ui_theme::DISABLED_TEXT
                } else {
                    TEXT
                },
            );
            SetBkColor(hdc, ui_theme::SURFACE);
            return GetStockObject(WHITE_BRUSH) as isize;
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            let id = GetDlgCtrlID(lparam as HWND);
            let color = match id {
                ID_VALUE => ACCENT,
                id if id == ID_HOTKEY_INPUT
                    || (ID_PRESET_INPUT..ID_PRESET_INPUT + 5).contains(&id) =>
                {
                    if IsWindowEnabled(lparam as HWND) == 0 {
                        ui_theme::DISABLED_TEXT
                    } else {
                        TEXT
                    }
                }
                ID_TARGET | ID_CHANGED_TITLE | ID_SETTINGS_TITLE | ID_BASIC_TITLE
                | ID_HOTKEY_TITLE | ID_PRESETS_TITLE => TEXT,
                _ => MUTED,
            };
            SetTextColor(hdc, color);
            SetBkColor(hdc, 0x00ffffff);
            return GetStockObject(WHITE_BRUSH) as isize;
        }
        WM_SETCURSOR if owns_picker_capture(hwnd) => {
            SetCursor(LoadCursorW(null_mut(), IDC_CROSS));
            return 1;
        }
        APP_EVENT if wparam == EVENT_DESTROY => {
            DestroyWindow(hwnd);
            return 0;
        }
        _ => {}
    }

    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if !pointer.is_null() {
        if message == APP_EVENT && wparam == EVENT_SHOW_ERROR {
            let error = (*pointer)
                .try_borrow_mut()
                .ok()
                .and_then(|mut app| app.pending_error.take());
            if let Some(error) = error {
                // 显示模态窗口前释放状态借用，让嵌套消息循环正常处理事件
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
                MessageBoxW(
                    hwnd,
                    wide(&error).as_ptr(),
                    wide(tr("Trans · 提示", "Trans · Notice")).as_ptr(),
                    MB_OK | MB_ICONWARNING,
                );
            }
            return 0;
        }
        if message == APP_EVENT && wparam == EVENT_SHOW_MENU {
            let request = (*pointer).try_borrow_mut().ok().and_then(|mut app| {
                if app.popup_open || app.exiting {
                    return None;
                }
                let request = app.pending_menu.take()?;
                app.popup_open = true;
                Some(request)
            });
            if let Some(request) = request {
                // 菜单会运行嵌套消息循环，必须释放状态借用后再显示
                let command = show_popup(hwnd, &request);
                if let Ok(mut app) = (*pointer).try_borrow_mut() {
                    app.popup_open = false;
                    if IsWindow(hwnd) != 0 {
                        match command {
                            Ok(command) => app.menu_command(request, command),
                            Err(error) => app.report_error(&error),
                        }
                    }
                }
            }
            return 0;
        }
        // 控件可能同步回调父窗口，嵌套消息不重复借用应用状态
        if let Ok(mut app) = (*pointer).try_borrow_mut() {
            match message {
                WM_CREATE => {
                    if let Err(error) = app.initialize(hwnd) {
                        app.initialization_error = Some(error);
                        return -1;
                    }
                    return 0;
                }
                APP_EVENT => {
                    app.event(wparam, lparam);
                    return 0;
                }
                WM_CONTEXTMENU if wparam as HWND == app.control(ID_CHANGED_LIST) => {
                    app.queue_window_menu(wparam as HWND, lparam);
                    return 0;
                }
                tray::CALLBACK_MESSAGE => {
                    let event = lparam as u32 & 0xffff;
                    let icon = (lparam as u32 >> 16) & 0xffff;
                    if icon == tray::TRAY_ID {
                        if event == NIN_SELECT
                            || event == (NIN_SELECT | NINF_KEY)
                            || event == WM_LBUTTONUP
                            || event == WM_LBUTTONDBLCLK
                        {
                            post(hwnd, EVENT_SHOW_MAIN, 0);
                        } else if event == WM_CONTEXTMENU || event == WM_RBUTTONUP {
                            let mut point = screen_point(wparam);
                            if point.x == -1 && point.y == -1 {
                                GetCursorPos(&mut point);
                            }
                            app.queue_menu(PopupRequest::Tray { point });
                        }
                    }
                    return 0;
                }
                message if app.taskbar_created != 0 && message == app.taskbar_created => {
                    post(hwnd, EVENT_TRAY_READD, 0);
                    return 0;
                }
                WM_ENDSESSION if wparam != 0 => {
                    let _ = app.save_current_settings();
                    app.window_events = None;
                    KillTimer(hwnd, TIMER_TARGET_ACTIVITY);
                    app.exiting = true;
                    app.controller.restore_all();
                    return 0;
                }
                WM_WINDOWPOSCHANGED => {
                    app.remember_main_window_geometry();
                }
                _ => {}
            }
        }
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}
