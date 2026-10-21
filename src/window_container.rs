use std::cell::{Cell, RefCell};
use std::mem::{size_of, size_of_val, zeroed};
use std::ptr::null_mut;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMNCRP_ENABLED,
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_NCRENDERING_ENABLED, DWMWA_NCRENDERING_POLICY,
    DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetMonitorInfoW, GetStockObject, MapWindowPoints,
    MonitorFromRect, RedrawWindow, SetDCBrushColor, CLR_INVALID, DC_BRUSH, HDC, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetDpiForWindow, GetWindowDpiAwarenessContext,
    PhysicalToLogicalPointForPerMonitorDPI, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetCapture, SetFocus};
use windows_sys::Win32::UI::Shell::ExtractIconExW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::container_frame::{self, ContainerFrame};
use crate::localization::tr;
use crate::taskbar::ContainerTaskbarGroup;
use crate::window_control::{restore_window_topmost, window_is_topmost, WindowInfo};
use crate::window_topmost;

static CONTAINER_CLASS: OnceLock<Result<(), u32>> = OnceLock::new();
static HOST_PROPERTY: OnceLock<Vec<u16>> = OnceLock::new();
static NEXT_HOST_NONCE: AtomicUsize = AtomicUsize::new(1);
const WM_COMPATIBILITY_FOCUS: u32 = WM_APP + 1;
// 仅作为宿主客户区的透明色，不参与原应用的绘制
const COMPATIBILITY_COLOR_KEY: u32 = 0x0003_0201;

pub struct ContainerAttachError {
    pub message: String,
    pub container: Option<WindowContainer>,
}

impl From<String> for ContainerAttachError {
    fn from(message: String) -> Self {
        Self {
            message,
            container: None,
        }
    }
}

pub struct WindowContainer {
    hwnd: HWND,
    state_ptr: *const ContainerState,
    owned_state: Rc<ContainerState>,
    nonce: usize,
    thread_id: u32,
}

struct ContainerState {
    host_nonce: usize,
    target: WindowInfo,
    property: Vec<u16>,
    token: usize,
    original_style: u32,
    original_ex_style: u32,
    original_owner: HWND,
    original_placement: WINDOWPLACEMENT,
    original_rect: RECT,
    mode: HostingMode,
    original_layered: Option<LayeredState>,
    original_nc_rendering: Option<bool>,
    nc_rendering_modified: Cell<bool>,
    original_corner_preference: Option<i32>,
    corner_preference_modified: Cell<bool>,
    topmost_modified: Cell<bool>,
    compatibility_layout_pending: Cell<bool>,
    compatibility_restore_pending: Cell<bool>,
    caption: ContainerFrame,
    frame_insets: FrameInsets,
    dpi_context: DPI_AWARENESS_CONTEXT,
    needs_restore: Cell<bool>,
    restoring: Cell<bool>,
    resizing: Cell<bool>,
    mirroring_target_state: Cell<bool>,
    host_animation_reset_pending: Cell<bool>,
    layout_request: Cell<Option<[i32; 4]>>,
    target_was_visible: Cell<bool>,
    target_was_minimized: Cell<bool>,
    target_was_maximized: Cell<bool>,
    target_restore_pending: Cell<bool>,
    activation_pending: Cell<bool>,
    transparency: Cell<u8>,
    title: RefCell<String>,
    icons: ContainerIcons,
    taskbar_group: RefCell<Option<ContainerTaskbarGroup>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostingMode {
    Embedded,
    // 保持 GPU 根窗口为顶层窗口，宿主仅提供外框和所有者关系
    TopLevel,
}

#[derive(Clone, Copy)]
struct LayeredState {
    color_key: u32,
    alpha: u8,
    flags: u32,
}

impl LayeredState {
    unsafe fn capture(target: HWND) -> Result<Self, String> {
        let mut value = Self {
            color_key: 0,
            alpha: 0,
            flags: 0,
        };
        if GetLayeredWindowAttributes(
            target,
            &mut value.color_key,
            &mut value.alpha,
            &mut value.flags,
        ) == 0
        {
            return Err(last_error(tr(
                "无法读取窗口原始分层属性，已保留原状态",
                "Cannot read the original layered attributes; the window was left unchanged",
            )));
        }
        Ok(value)
    }
}

#[derive(Clone, Copy, Default)]
struct FrameInsets {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    dpi: u32,
    maximized: bool,
}

impl FrameInsets {
    unsafe fn capture(target: HWND, window: RECT) -> Self {
        if IsIconic(target) != 0 || IsWindowVisible(target) == 0 {
            return Self::default();
        }
        let mut visible: RECT = zeroed();
        if DwmGetWindowAttribute(
            target,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut visible as *mut RECT as _,
            size_of::<RECT>() as u32,
        ) < 0
        {
            return Self::default();
        }
        let mut start = POINT {
            x: visible.left,
            y: visible.top,
        };
        let mut end = POINT {
            x: visible.right,
            y: visible.bottom,
        };
        // DWM 返回物理坐标，先换算到目标 DPI 模式再比较窗口边界
        if PhysicalToLogicalPointForPerMonitorDPI(target, &mut start) == 0
            || PhysicalToLogicalPointForPerMonitorDPI(target, &mut end) == 0
        {
            return Self::default();
        }
        let dpi = GetDpiForWindow(target).max(96);
        let maximum = (32 * dpi as i32 / 96).max(8);
        let edges = [
            start.x - window.left,
            start.y - window.top,
            window.right - end.x,
            window.bottom - end.y,
        ];
        if start.x >= end.x
            || start.y >= end.y
            || edges.iter().any(|edge| *edge < 0 || *edge > maximum)
        {
            return Self::default();
        }
        Self {
            left: edges[0],
            top: edges[1],
            right: edges[2],
            bottom: edges[3],
            dpi,
            maximized: IsZoomed(target) != 0,
        }
    }

    fn at_dpi(self, dpi: u32) -> Self {
        let base = self.dpi.max(1);
        let scale =
            |value: i32| ((value as i64 * dpi as i64 + base as i64 / 2) / base as i64) as i32;
        Self {
            left: scale(self.left),
            top: scale(self.top),
            right: scale(self.right),
            bottom: scale(self.bottom),
            dpi,
            maximized: self.maximized,
        }
    }

    fn visible_rect(self, window: RECT) -> RECT {
        RECT {
            left: window.left + self.left,
            top: window.top + self.top,
            right: window.right - self.right,
            bottom: window.bottom - self.bottom,
        }
    }

    unsafe fn for_layout(mut self, target: HWND, window: RECT) -> Self {
        if !self.maximized || IsZoomed(target) != 0 {
            return self;
        }
        // 最大化的隐藏边距不能直接用于恢复后的自绘标题栏，以当前非客户区限定
        let mut client: RECT = zeroed();
        if GetClientRect(target, &mut client) == 0 {
            return Self::default();
        }
        SetLastError(0);
        if MapWindowPoints(
            target,
            null_mut(),
            &mut client as *mut RECT as *mut POINT,
            2,
        ) == 0
            && GetLastError() != 0
        {
            return Self::default();
        }
        self.left = self.left.min((client.left - window.left).max(0));
        self.top = self.top.min((client.top - window.top).max(0));
        self.right = self.right.min((window.right - client.right).max(0));
        self.bottom = self.bottom.min((window.bottom - client.bottom).max(0));
        self
    }
}

#[derive(Default)]
struct OwnedIcon(HICON);

impl OwnedIcon {
    unsafe fn copy(icon: HICON) -> Self {
        if icon.is_null() {
            Self::default()
        } else {
            Self(CopyIcon(icon))
        }
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyIcon(self.0) };
        }
    }
}

#[derive(Default)]
struct ContainerIcons {
    small: OwnedIcon,
    large: OwnedIcon,
}

impl ContainerIcons {
    unsafe fn capture(state: &ContainerState) -> Result<Self, String> {
        state.validate()?;
        let started = Instant::now();
        let mut responsive = true;
        let dpi = GetDpiForWindow(state.target.hwnd);
        let mut icons = Self {
            large: query_icon(state, ICON_BIG, dpi, &started, &mut responsive)?,
            small: query_icon(state, ICON_SMALL, dpi, &started, &mut responsive)?,
        };

        for (icon, slot) in [
            (&mut icons.large, GCLP_HICON),
            (&mut icons.small, GCLP_HICONSM),
        ] {
            if icon.0.is_null() {
                state.validate()?;
                *icon = OwnedIcon::copy(GetClassLongPtrW(state.target.hwnd, slot) as HICON);
                state.validate()?;
            }
        }

        if icons.small.0.is_null() || icons.large.0.is_null() {
            // 窗口不提供图标时，读取所属程序资源，避免容器使用系统占位图标
            state.validate()?;
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                state.target.process_id,
            );
            if !process.is_null() {
                let mut path = vec![0u16; 32768];
                let mut length = (path.len() - 1) as u32;
                let found = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length);
                CloseHandle(process);
                state.validate()?;
                if found != 0 && length > 0 && (length as usize) < path.len() {
                    path[length as usize] = 0;
                    let mut extracted = Self::default();
                    let count = ExtractIconExW(
                        path.as_ptr(),
                        0,
                        &mut extracted.large.0,
                        &mut extracted.small.0,
                        1,
                    );
                    state.validate()?;
                    if count != 0 && count != u32::MAX {
                        if icons.small.0.is_null() {
                            icons.small = std::mem::take(&mut extracted.small);
                        }
                        if icons.large.0.is_null() {
                            icons.large = std::mem::take(&mut extracted.large);
                        }
                    }
                }
            }
        }

        if icons.small.0.is_null() {
            icons.small = OwnedIcon::copy(icons.large.0);
        }
        // SMALL2 可能返回系统生成的占位图标，留到已知图标和程序资源之后再尝试
        if icons.small.0.is_null() {
            icons.small = query_icon(state, ICON_SMALL2, dpi, &started, &mut responsive)?;
        }
        if icons.large.0.is_null() {
            icons.large = OwnedIcon::copy(icons.small.0);
        }
        state.validate()?;
        Ok(icons)
    }

    unsafe fn apply(&self, host: HWND) {
        for (kind, icon) in [(ICON_SMALL, &self.small), (ICON_BIG, &self.large)] {
            if !icon.0.is_null() {
                SendMessageW(host, WM_SETICON, kind as usize, icon.0 as isize);
            }
        }
    }
}

unsafe fn query_icon(
    state: &ContainerState,
    kind: u32,
    dpi: u32,
    started: &Instant,
    responsive: &mut bool,
) -> Result<OwnedIcon, String> {
    state.validate()?;
    let remaining =
        60u32.saturating_sub(started.elapsed().as_millis().min(u32::MAX as u128) as u32);
    if !*responsive || remaining == 0 {
        return Ok(OwnedIcon::default());
    }
    let mut icon = 0;
    let result = SendMessageTimeoutW(
        state.target.hwnd,
        WM_GETICON,
        kind as usize,
        dpi as isize,
        SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
        remaining,
        &mut icon,
    );
    state.validate()?;
    if result == 0 {
        *responsive = false;
        return Ok(OwnedIcon::default());
    }
    Ok(OwnedIcon::copy(icon as HICON))
}

struct DpiScope(DPI_AWARENESS_CONTEXT);

impl DpiScope {
    unsafe fn enter(context: DPI_AWARENESS_CONTEXT) -> Result<Self, String> {
        let previous = SetThreadDpiAwarenessContext(context);
        if previous.is_null() {
            return Err(last_error(tr(
                "无法匹配目标窗口 DPI 模式",
                "Cannot match the target window DPI awareness",
            )));
        }
        Ok(Self(previous))
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

#[derive(Clone, Copy)]
struct HostIdentity {
    hwnd: HWND,
    state_ptr: *const ContainerState,
    nonce: usize,
    thread_id: u32,
}

impl HostIdentity {
    unsafe fn matches(&self) -> bool {
        if self.hwnd.is_null()
            || self.state_ptr.is_null()
            || GetCurrentThreadId() != self.thread_id
            || IsWindow(self.hwnd) == 0
        {
            return false;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(self.hwnd, &mut process_id) == self.thread_id
            && process_id == GetCurrentProcessId()
            && GetPropW(self.hwnd, host_property().as_ptr()) as usize == self.nonce
            && GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *const ContainerState
                == self.state_ptr
            && GetWindowLongPtrW(self.hwnd, GWLP_WNDPROC) == container_proc as *const () as isize
            && (*self.state_ptr).host_nonce == self.nonce
    }
}

struct HostAnimationScope {
    identity: HostIdentity,
    transitions_disabled: bool,
    active: bool,
}

impl HostAnimationScope {
    unsafe fn begin(container: &WindowContainer) -> Result<Self, String> {
        let identity = HostIdentity {
            hwnd: container.hwnd,
            state_ptr: container.state_ptr,
            nonce: container.nonce,
            thread_id: container.thread_id,
        };
        if !identity.matches() {
            return Err(tr(
                "透明容器身份已变化，无法更新窗口动画",
                "The transparency container identity changed; its animation cannot be updated safely",
            )
            .into());
        }
        let state = container.state()?;
        if state.mirroring_target_state.replace(true) {
            return Err(tr(
                "容器正在同步窗口状态，请稍后重试",
                "The container is synchronizing its window state; try again shortly",
            )
            .into());
        }
        let mut scope = Self {
            identity,
            transitions_disabled: false,
            active: true,
        };
        set_host_transitions_disabled(identity.hwnd, true)?;
        scope.transitions_disabled = true;
        if identity.matches() {
            (*identity.state_ptr).host_animation_reset_pending.set(true);
        }
        Ok(scope)
    }

    unsafe fn restore_transitions(&mut self) -> Result<(), String> {
        if !self.active || !self.identity.matches() {
            self.active = false;
            return Ok(());
        }
        if self.transitions_disabled {
            set_host_transitions_disabled(self.identity.hwnd, false)?;
            self.transitions_disabled = false;
            if self.identity.matches() {
                (*self.identity.state_ptr)
                    .host_animation_reset_pending
                    .set(false);
            }
        }
        Ok(())
    }

    unsafe fn release(&mut self) {
        if self.active && self.identity.matches() {
            (*self.identity.state_ptr).mirroring_target_state.set(false);
        }
        self.active = false;
    }

    unsafe fn finish(mut self) -> Result<(), String> {
        let result = self.restore_transitions();
        if result.is_ok() {
            self.release();
        }
        result
    }
}

impl Drop for HostAnimationScope {
    fn drop(&mut self) {
        unsafe {
            let _ = self.restore_transitions();
            self.release();
        }
    }
}

unsafe fn set_host_transitions_disabled(host: HWND, disabled: bool) -> Result<(), String> {
    let disabled: i32 = i32::from(disabled);
    let result = DwmSetWindowAttribute(
        host,
        DWMWA_TRANSITIONS_FORCEDISABLED as u32,
        &disabled as *const i32 as _,
        size_of::<i32>() as u32,
    );
    if result < 0 {
        return Err(format!(
            "{} (HRESULT 0x{:08X})",
            tr(
                "无法更新容器窗口动画策略",
                "Cannot update the container window animation policy",
            ),
            result as u32
        ));
    }
    Ok(())
}

impl ContainerState {
    unsafe fn target_exists(&self) -> bool {
        if IsWindow(self.target.hwnd) == 0 {
            return false;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(self.target.hwnd, &mut process_id);
        process_id == self.target.process_id && thread_id == self.target.thread_id
    }

    unsafe fn target_identity_gone(&self) -> bool {
        if IsWindow(self.target.hwnd) == 0 {
            return true;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(self.target.hwnd, &mut process_id);
        if thread_id == 0 || process_id == 0 {
            return IsWindow(self.target.hwnd) == 0;
        }
        process_id != self.target.process_id || thread_id != self.target.thread_id
    }

    unsafe fn validate(&self) -> Result<(), String> {
        if !self.target_exists()
            || GetPropW(self.target.hwnd, self.property.as_ptr()) as usize != self.token
        {
            return Err(tr(
                "容器中的窗口已关闭或身份标记发生变化，无法安全修改",
                "The hosted window has closed or its ownership marker changed; it cannot be modified safely",
            )
            .into());
        }
        Ok(())
    }

    unsafe fn set_target_long(&self, index: i32, value: isize) -> Result<(), String> {
        self.validate()?;
        set_window_long(self.target.hwnd, index, value)
    }

    unsafe fn attached_to(&self, host: HWND) -> bool {
        let child = GetWindowLongPtrW(self.target.hwnd, GWL_STYLE) as u32 & WS_CHILD != 0;
        match self.mode {
            HostingMode::Embedded => child && GetParent(self.target.hwnd) == host,
            HostingMode::TopLevel => !child && GetWindow(self.target.hwnd, GW_OWNER) == host,
        }
    }

    unsafe fn validate_host(&self, host: HWND) -> Result<(), String> {
        let identity = HostIdentity {
            hwnd: host,
            state_ptr: self,
            nonce: self.host_nonce,
            thread_id: GetCurrentThreadId(),
        };
        if !identity.matches() {
            return Err(tr(
                "透明容器身份已变化，无法继续同步",
                "The transparency container identity changed; synchronization was stopped",
            )
            .into());
        }
        Ok(())
    }

    unsafe fn apply_compatibility_alpha(&self, percent: u8) -> Result<(), String> {
        self.validate()?;
        let ex_style = read_extended_style(self.target.hwnd)?;
        let alpha = (((100 - percent.min(100) as u32) * 255 + 50) / 100) as u8;
        let color_key = self.original_layered.map_or(0, |value| value.color_key);
        let flags = LWA_ALPHA
            | self
                .original_layered
                .map_or(0, |value| value.flags & LWA_COLORKEY);
        if ex_style & WS_EX_LAYERED != 0 {
            if let Ok(current) = LayeredState::capture(self.target.hwnd) {
                if current.alpha == alpha
                    && current.flags == flags
                    && current.color_key == color_key
                {
                    return Ok(());
                }
            }
        } else {
            // 保留 NOREDIRECTIONBITMAP，只使用直接模式已支持的根窗口透明度
            self.set_target_long(GWL_EXSTYLE, (ex_style | WS_EX_LAYERED) as isize)?;
        }
        self.validate()?;
        if SetLayeredWindowAttributes(self.target.hwnd, color_key, alpha, flags) == 0 {
            return Err(last_error(tr(
                "无法设置兼容容器内容透明度",
                "Cannot set the compatibility container content opacity",
            )));
        }
        Ok(())
    }

    unsafe fn set_target_nc_rendering(&self, enabled: bool) -> Result<(), String> {
        self.validate()?;
        let policy = if enabled {
            DWMNCRP_ENABLED
        } else {
            DWMNCRP_DISABLED
        };
        let result = DwmSetWindowAttribute(
            self.target.hwnd,
            DWMWA_NCRENDERING_POLICY as u32,
            &policy as *const _ as _,
            size_of_val(&policy) as u32,
        );
        if result < 0 {
            return Err(format!(
                "{} (HRESULT 0x{:08X})",
                tr(
                    "无法更新原窗口的非客户区绘制状态",
                    "Cannot update the original window's non-client rendering state"
                ),
                result as u32,
            ));
        }
        self.validate()
    }

    unsafe fn suppress_target_shadow(&self) -> Result<(), String> {
        if self.original_nc_rendering.is_none() {
            return Ok(());
        }
        self.validate()?;
        let enabled = read_nc_rendering(self.target.hwnd)?;
        self.validate()?;
        if enabled {
            // 顶层目标的 DWM 阴影会覆盖宿主标题栏，先登记恢复责任再关闭
            self.nc_rendering_modified.set(true);
            self.set_target_nc_rendering(false)?;
        }
        Ok(())
    }

    unsafe fn restore_target_nc_rendering(&self) -> Result<(), String> {
        if self.nc_rendering_modified.get() {
            if let Some(enabled) = self.original_nc_rendering {
                // 原策略枚举不支持读取，恢复保存的有效启用状态
                self.set_target_nc_rendering(enabled)?;
                self.nc_rendering_modified.set(false);
            }
        }
        Ok(())
    }

    unsafe fn set_target_corner_preference(&self, preference: i32) -> Result<(), String> {
        self.validate()?;
        let result = DwmSetWindowAttribute(
            self.target.hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &preference as *const _ as _,
            size_of_val(&preference) as u32,
        );
        if result < 0 {
            return Err(format!(
                "{} (HRESULT 0x{:08X})",
                tr(
                    "无法更新原窗口的圆角偏好",
                    "Cannot update the original window's corner preference"
                ),
                result as u32,
            ));
        }
        self.validate()
    }

    unsafe fn suppress_target_corners(&self) -> Result<(), String> {
        if self.original_corner_preference.is_none() {
            return Ok(());
        }
        self.validate()?;
        if let Some(current) = read_corner_preference(self.target.hwnd) {
            if current != DWMWCP_DONOTROUND {
                // 内层圆角会在接缝处透出背景，先登记恢复责任再请求直角
                self.corner_preference_modified.set(true);
                self.set_target_corner_preference(DWMWCP_DONOTROUND)?;
            }
        }
        self.validate()
    }

    unsafe fn restore_target_corner_preference(&self) -> Result<(), String> {
        if self.corner_preference_modified.get() {
            if let Some(preference) = self.original_corner_preference {
                self.set_target_corner_preference(preference)?;
                self.corner_preference_modified.set(false);
            }
        }
        Ok(())
    }

    unsafe fn suppress_target_frame(&self) -> Result<(), String> {
        self.validate()?;
        let style = GetWindowLongPtrW(self.target.hwnd, GWL_STYLE) as u32;
        let frame = WS_CAPTION | WS_THICKFRAME;
        if style & frame == 0 {
            return Ok(());
        }
        // 外框承担标题栏和缩放边缘，原窗口不再保留会透底的原生边框
        self.set_target_long(GWL_STYLE, (style & !frame) as isize)?;
        self.validate()?;
        if SetWindowPos(
            self.target.hwnd,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        ) == 0
        {
            return Err(last_error(tr(
                "无法更新原窗口的容器边框",
                "Cannot update the hosted window frame",
            )));
        }
        self.validate()
    }

    unsafe fn initialize_container_frame(&self, host: HWND) -> Result<(), String> {
        self.validate_host(host)?;
        // 两种托管方式共用平面外框，窗口组织方式不决定标题栏外观
        let policy = DWMNCRP_DISABLED;
        let result = DwmSetWindowAttribute(
            host,
            DWMWA_NCRENDERING_POLICY as u32,
            &policy as *const _ as _,
            size_of_val(&policy) as u32,
        );
        if result < 0 {
            return Err(format!(
                "{} (HRESULT 0x{:08X})",
                tr(
                    "无法初始化容器平面边框",
                    "Cannot initialize the container's flat frame"
                ),
                result as u32,
            ));
        }
        self.validate_host(host)?;
        // 圆角仅作系统提示，Windows 10 等不支持时沿用默认外观
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            host,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &corner as *const _ as _,
            size_of_val(&corner) as u32,
        );
        self.validate_host(host)?;
        if SetWindowPos(
            host,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) == 0
        {
            return Err(last_error(tr(
                "无法更新容器边距",
                "Cannot update the container frame insets",
            )));
        }
        self.validate_host(host)?;
        if RedrawWindow(
            host,
            null_mut(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME,
        ) == 0
        {
            return Err(last_error(tr(
                "无法刷新容器外观",
                "Cannot refresh the container frame",
            )));
        }
        self.validate_host(host)
    }

    unsafe fn layout_top_level(&self, host: HWND) -> Result<(), String> {
        self.validate_host(host)?;
        if IsZoomed(self.target.hwnd) != 0 || self.compatibility_restore_pending.get() {
            return Ok(());
        }
        let (_, client) = host_bounds(host)?;
        let mut target: RECT = zeroed();
        if GetWindowRect(self.target.hwnd, &mut target) == 0 {
            return Err(last_error(tr(
                "无法读取目标窗口位置",
                "Cannot read the target window bounds",
            )));
        }
        // 抑制目标非客户区后重新读取可见边界，不沿用进入容器前的隐藏边距
        let insets =
            FrameInsets::capture(self.target.hwnd, target).for_layout(self.target.hwnd, target);
        let requested = [
            client.left - insets.left,
            client.top - insets.top,
            client.right - client.left + insets.left + insets.right,
            client.bottom - client.top + insets.top + insets.bottom,
        ];
        if requested[2] <= 0 || requested[3] <= 0 {
            return Ok(());
        }
        self.validate()?;
        if target.left != requested[0]
            || target.top != requested[1]
            || target.right - target.left != requested[2]
            || target.bottom - target.top != requested[3]
        {
            if SetWindowPos(
                self.target.hwnd,
                null_mut(),
                requested[0],
                requested[1],
                requested[2],
                requested[3],
                SWP_NOACTIVATE | SWP_NOZORDER,
            ) == 0
            {
                return Err(last_error(tr(
                    "无法同步兼容容器位置",
                    "Cannot position the compatibility container content",
                )));
            }
        }
        self.layout_request.set(Some(requested));
        self.compatibility_layout_pending.set(false);
        self.validate_host(host)?;
        self.validate()
    }

    unsafe fn follow_top_level(&self, host: HWND) -> Result<(), String> {
        self.validate_host(host)?;
        let mut target: RECT = zeroed();
        if GetWindowRect(self.target.hwnd, &mut target) == 0 {
            return Err(last_error(tr(
                "无法读取目标窗口位置",
                "Cannot read the target window bounds",
            )));
        }
        let insets =
            FrameInsets::capture(self.target.hwnd, target).for_layout(self.target.hwnd, target);
        let visible = insets.visible_rect(target);
        let (_, client) = host_bounds(host)?;
        if client.left != visible.left
            || client.top != visible.top
            || client.right != visible.right
            || client.bottom != visible.bottom
        {
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: (visible.right - visible.left).max(1),
                bottom: (visible.bottom - visible.top).max(1),
            };
            container_frame::expand_client(&mut frame, GetDpiForWindow(host).max(96));
            // 原窗口自行移动或限制尺寸时让外框跟随，不反复将它拉回旧位置
            self.mirroring_target_state.set(true);
            let moved = SetWindowPos(
                host,
                null_mut(),
                visible.left + frame.left,
                visible.top + frame.top,
                frame.right - frame.left,
                frame.bottom - frame.top,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            self.mirroring_target_state.set(false);
            if moved == 0 {
                return Err(last_error(tr(
                    "无法同步兼容容器外框",
                    "Cannot synchronize the compatibility container frame",
                )));
            }
        }
        self.validate_host(host)
    }

    unsafe fn resize_target(&self, host: HWND) -> Result<(), String> {
        if self.restoring.get()
            || window_topmost::is_pending(host)
            || window_topmost::is_pending(self.target.hwnd)
            || self.mirroring_target_state.get()
            || IsIconic(host) != 0
            || IsIconic(self.target.hwnd) != 0
            || self.resizing.replace(true)
        {
            return Ok(());
        }
        let result = (|| {
            self.validate()?;
            let _dpi_scope = DpiScope::enter(self.dpi_context)?;
            if !self.attached_to(host) {
                return Err(tr(
                    "目标窗口已离开透明容器，请恢复后重新添加",
                    "The target window left its transparency container; restore and add it again",
                )
                .into());
            }
            if self.mode == HostingMode::TopLevel {
                return self.layout_top_level(host);
            }
            let mut rect: RECT = zeroed();
            if GetClientRect(host, &mut rect) == 0 {
                return Err(last_error(tr(
                    "无法读取容器尺寸",
                    "Cannot read the container size",
                )));
            }
            if rect.right <= rect.left || rect.bottom <= rect.top {
                return Ok(());
            }
            let mut target_rect: RECT = zeroed();
            if GetWindowRect(self.target.hwnd, &mut target_rect) == 0 {
                return Err(last_error(tr(
                    "无法读取容器内窗口尺寸",
                    "Cannot read the hosted window size",
                )));
            }
            let insets = self
                .frame_insets
                .at_dpi(GetDpiForWindow(host).max(96))
                .for_layout(self.target.hwnd, target_rect);
            let left = -insets.left;
            let top = -insets.top;
            let width = (rect.right - rect.left + insets.left + insets.right).max(1);
            let height = (rect.bottom - rect.top + insets.top + insets.bottom).max(1);
            SetLastError(0);
            if MapWindowPoints(
                null_mut(),
                host,
                &mut target_rect as *mut RECT as *mut POINT,
                2,
            ) == 0
                && GetLastError() != 0
            {
                return Err(last_error(tr(
                    "无法换算容器内窗口位置",
                    "Cannot map the hosted window position",
                )));
            }
            if target_rect.left == left
                && target_rect.top == top
                && target_rect.right - target_rect.left == width
                && target_rect.bottom - target_rect.top == height
            {
                return Ok(());
            }
            let requested = [left, top, width, height];
            // 目标可能限制最小尺寸，同一次布局只请求一次，避免位置事件反复重试
            if self.layout_request.get() == Some(requested) {
                return Ok(());
            }
            self.layout_request.set(Some(requested));
            self.validate()?;
            if SetWindowPos(
                self.target.hwnd,
                null_mut(),
                left,
                top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) == 0
            {
                self.layout_request.set(None);
                return Err(last_error(tr(
                    "无法调整容器内窗口大小",
                    "Cannot resize the hosted window",
                )));
            }
            Ok(())
        })();
        self.resizing.set(false);
        result
    }

    unsafe fn restore_target(&self, retry: bool) -> Result<(), String> {
        self.validate()?;
        let minimized = IsIconic(self.target.hwnd) != 0;
        let visible = GetWindowLongPtrW(self.target.hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0;
        let was_pending = self.target_restore_pending.get();
        if minimized && was_pending && !retry {
            return Ok(());
        }
        if minimized || !visible {
            self.target_restore_pending.set(true);
            if ShowWindowAsync(
                self.target.hwnd,
                if minimized { SW_RESTORE } else { SW_SHOWNA },
            ) == 0
            {
                self.target_restore_pending.set(was_pending);
                return Err(last_error(tr(
                    "无法显示容器内窗口",
                    "Cannot show the hosted window",
                )));
            }
        }
        Ok(())
    }

    unsafe fn finish_activation(&self, host: HWND) -> Result<(), String> {
        if !self.activation_pending.get()
            || self.resizing.get()
            || self.restoring.get()
            || self.mirroring_target_state.get()
        {
            return Ok(());
        }
        self.validate()?;
        if IsWindowVisible(host) == 0
            || IsIconic(host) != 0
            || IsIconic(self.target.hwnd) != 0
            || GetWindowLongPtrW(self.target.hwnd, GWL_STYLE) as u32 & WS_VISIBLE == 0
        {
            return Ok(());
        }
        // 原窗口异步显示完成后补齐布局，不能仅依赖宿主是否收到 WM_SIZE
        self.layout_request.set(None);
        self.resize_target(host)?;
        self.validate()?;
        if RedrawWindow(
            self.target.hwnd,
            null_mut(),
            null_mut(),
            RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
        ) == 0
        {
            return Err(last_error(tr(
                "无法刷新容器内窗口",
                "Cannot refresh the hosted window",
            )));
        }
        self.activation_pending.set(false);
        if GetForegroundWindow() == host {
            if self.mode == HostingMode::TopLevel {
                SetForegroundWindow(self.target.hwnd);
            } else {
                SetFocus(self.target.hwnd);
            }
        }
        Ok(())
    }
}

impl WindowContainer {
    pub fn check_render_compatibility(window: &WindowInfo) -> Result<(), String> {
        Self::hosting_mode(window).map(|_| ())
    }

    pub fn supports_rendering(window: &WindowInfo) -> Result<bool, String> {
        Self::check_render_compatibility(window).map(|_| true)
    }

    fn hosting_mode(window: &WindowInfo) -> Result<HostingMode, String> {
        unsafe {
            if read_window_class(window.hwnd)? == "Chrome_WidgetWin_1"
                && read_extended_style(window.hwnd)? & WS_EX_NOREDIRECTIONBITMAP != 0
            {
                // 不再强制嵌入这一绘制组合，也不依赖特定程序名或 GPU 子窗口寿命
                Ok(HostingMode::TopLevel)
            } else {
                Ok(HostingMode::Embedded)
            }
        }
    }

    pub fn attach(
        window: &WindowInfo,
        ownership_property: &[u16],
        token: usize,
        transparency: u8,
    ) -> Result<Self, ContainerAttachError> {
        unsafe {
            if ownership_property.last() != Some(&0) || token == 0 || transparency > 100 {
                return Err(tr(
                    "透明容器参数无效",
                    "Invalid transparency container parameters",
                )
                .to_string()
                .into());
            }
            let mut placement: WINDOWPLACEMENT = zeroed();
            placement.length = size_of::<WINDOWPLACEMENT>() as u32;
            let mut original_rect: RECT = zeroed();
            let nonce = NEXT_HOST_NONCE.fetch_add(1, Ordering::Relaxed);
            let mode = Self::hosting_mode(window)?;
            let original_ex_style = read_extended_style(window.hwnd)?;
            let original_layered =
                if mode == HostingMode::TopLevel && original_ex_style & WS_EX_LAYERED != 0 {
                    Some(LayeredState::capture(window.hwnd)?)
                } else {
                    None
                };
            let original_nc_rendering = if mode == HostingMode::TopLevel {
                Some(read_nc_rendering(window.hwnd)?)
            } else {
                None
            };
            // 能读取原值才临时调整，旧系统不支持时保留原窗口圆角
            let original_corner_preference = if mode == HostingMode::TopLevel {
                read_corner_preference(window.hwnd)
            } else {
                None
            };
            let state = Box::new(ContainerState {
                host_nonce: nonce,
                target: window.clone(),
                property: ownership_property.to_vec(),
                token,
                original_style: GetWindowLongPtrW(window.hwnd, GWL_STYLE) as u32,
                original_ex_style,
                original_owner: GetWindow(window.hwnd, GW_OWNER),
                original_placement: placement,
                original_rect,
                mode,
                original_layered,
                original_nc_rendering,
                nc_rendering_modified: Cell::new(false),
                original_corner_preference,
                corner_preference_modified: Cell::new(false),
                topmost_modified: Cell::new(false),
                compatibility_layout_pending: Cell::new(true),
                compatibility_restore_pending: Cell::new(false),
                caption: ContainerFrame::default(),
                frame_insets: FrameInsets::default(),
                dpi_context: GetWindowDpiAwarenessContext(window.hwnd),
                needs_restore: Cell::new(false),
                restoring: Cell::new(false),
                resizing: Cell::new(false),
                mirroring_target_state: Cell::new(false),
                host_animation_reset_pending: Cell::new(false),
                layout_request: Cell::new(None),
                target_was_visible: Cell::new(false),
                target_was_minimized: Cell::new(false),
                target_was_maximized: Cell::new(false),
                target_restore_pending: Cell::new(false),
                activation_pending: Cell::new(false),
                transparency: Cell::new(0),
                title: RefCell::new(window.title.clone()),
                icons: ContainerIcons::default(),
                taskbar_group: RefCell::new(None),
            });
            state.validate()?;
            if state.dpi_context.is_null() {
                return Err(last_error(tr(
                    "无法读取目标窗口 DPI 模式",
                    "Cannot read the target window DPI awareness",
                ))
                .into());
            }
            let _dpi_scope = DpiScope::enter(state.dpi_context)?;
            if state.original_style & WS_CHILD != 0 {
                return Err(tr(
                    "此窗口已属于其他容器",
                    "This window already belongs to another container",
                )
                .to_string()
                .into());
            }
            if GetWindowPlacement(window.hwnd, &mut placement) == 0
                || GetWindowRect(window.hwnd, &mut original_rect) == 0
            {
                return Err(last_error(tr(
                    "无法保存原窗口位置",
                    "Cannot save the original window placement",
                ))
                .into());
            }
            let mut state = state;
            state.original_placement = placement;
            state.original_rect = original_rect;
            state.frame_insets = FrameInsets::capture(window.hwnd, original_rect);
            // 在改变父窗口和样式之前保留图标副本，由宿主状态持有至销毁
            state.icons = ContainerIcons::capture(&state)?;
            register_class()?;
            let target_dpi = state.dpi_context;
            let class_name = class_name();
            let title = wide(&window.title);
            let host_ex_style =
                WS_EX_APPWINDOW | WS_EX_LAYERED | (state.original_ex_style & WS_EX_TOPMOST);
            let host_style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
            let mut host_rect =
                state
                    .frame_insets
                    .visible_rect(if state.original_style & WS_MINIMIZE != 0 {
                        placement.rcNormalPosition
                    } else {
                        original_rect
                    });
            // 创建尺寸和非客户区布局共用边距，不挤占内部窗口的显示尺寸
            let monitor = MonitorFromRect(&host_rect, MONITOR_DEFAULTTONEAREST);
            let mut frame_rect = RECT {
                left: 0,
                top: 0,
                right: (host_rect.right - host_rect.left).max(160),
                bottom: (host_rect.bottom - host_rect.top).max(100),
            };
            container_frame::expand_client(&mut frame_rect, GetDpiForWindow(window.hwnd).max(96));
            let host_width = frame_rect.right - frame_rect.left;
            let host_height = frame_rect.bottom - frame_rect.top;
            let mut monitor_info: MONITORINFO = zeroed();
            monitor_info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut monitor_info) != 0 {
                // 扩出标题栏后将位置限制在工作区，避免顶部窗口失去可拖动区域
                let work = monitor_info.rcWork;
                host_rect.left = host_rect
                    .left
                    .clamp(work.left, (work.right - host_width).max(work.left));
                host_rect.top = host_rect
                    .top
                    .clamp(work.top, (work.bottom - host_height).max(work.top));
            }
            let host = CreateWindowExW(
                host_ex_style,
                class_name.as_ptr(),
                title.as_ptr(),
                host_style,
                host_rect.left,
                host_rect.top,
                host_width,
                host_height,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null_mut()),
                null_mut(),
            );
            if host.is_null() {
                return Err(last_error(tr(
                    "无法创建透明容器",
                    "Cannot create the transparency container",
                ))
                .into());
            }
            if AreDpiAwarenessContextsEqual(GetWindowDpiAwarenessContext(host), target_dpi) == 0 {
                DestroyWindow(host);
                return Err(tr(
                    "目标窗口与容器的 DPI 模式不一致，已取消嵌入",
                    "The target and container DPI awareness do not match; embedding was cancelled",
                )
                .to_string()
                .into());
            }
            // 控制器与宿主分别持有状态，窗口消息重入销毁宿主后调用栈仍可安全收尾
            let state: Rc<ContainerState> = Rc::from(state);
            let state_ptr = Rc::into_raw(state.clone());
            SetLastError(0);
            let previous = SetWindowLongPtrW(host, GWLP_USERDATA, state_ptr as isize);
            let code = GetLastError();
            if previous == 0 && code != 0 {
                let error = error_with_code(
                    tr(
                        "无法初始化透明容器",
                        "Cannot initialize the transparency container",
                    ),
                    code,
                );
                drop(Rc::from_raw(state_ptr));
                DestroyWindow(host);
                return Err(error.into());
            }
            if SetPropW(host, host_property().as_ptr(), nonce as HANDLE) == 0 {
                let error = last_error(tr(
                    "无法标记透明容器身份",
                    "Cannot mark the transparency container identity",
                ));
                DestroyWindow(host);
                return Err(error.into());
            }
            let mut container = Self {
                hwnd: host,
                state_ptr,
                owned_state: state,
                nonce,
                thread_id: GetCurrentThreadId(),
            };
            let result = container.attach_target(transparency);
            if let Err(error) = result {
                return match container.detach() {
                    Ok(()) => Err(error.into()),
                    Err(restore_error) => Err(ContainerAttachError {
                        message: format!("{error}\n{restore_error}"),
                        container: Some(container),
                    }),
                };
            }
            Ok(container)
        }
    }

    unsafe fn attach_target(&mut self, transparency: u8) -> Result<(), String> {
        let process_id = self.state()?.target.process_id;
        // COM 调用可能重入，完成后重新取得并核验状态，再修改目标窗口
        let taskbar_group = ContainerTaskbarGroup::new(self.hwnd, process_id)?;
        let state = self.state()?;
        state.validate()?;
        state.taskbar_group.replace(Some(taskbar_group));
        // 宿主状态就绪后统一初始化外框，再进入各自的目标托管流程
        state.initialize_container_frame(self.hwnd)?;
        if state.mode == HostingMode::TopLevel {
            // 透明度也可能先写入原窗口，失败时必须保留可恢复的原始快照
            state.needs_restore.set(true);
        }
        self.set_transparency(transparency)?;
        let state = self.state()?;
        state.validate()?;
        state.icons.apply(self.hwnd);
        state.needs_restore.set(true);
        if state.mode == HostingMode::TopLevel {
            state.set_target_long(GWLP_HWNDPARENT, self.hwnd as isize)?;
            state.set_target_long(
                GWL_EXSTYLE,
                (read_extended_style(state.target.hwnd)? & !WS_EX_APPWINDOW) as isize,
            )?;
            state.validate()?;
            if !state.attached_to(self.hwnd) {
                return Err(tr(
                    "目标窗口未关联兼容容器",
                    "The target window did not join its compatibility container",
                )
                .into());
            }
            state.suppress_target_shadow()?;
            state.suppress_target_corners()?;
            state.suppress_target_frame()?;
            if SetWindowPos(
                state.target.hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            ) == 0
            {
                return Err(last_error(tr(
                    "无法更新兼容窗口边框",
                    "Cannot update the compatibility window frame",
                )));
            }
            return self.sync();
        }
        let child_style =
            (state.original_style & !(WS_POPUP | WS_MAXIMIZE)) | WS_CHILD | WS_CLIPSIBLINGS;
        state.set_target_long(GWL_STYLE, child_style as isize)?;
        state.set_target_long(
            GWL_EXSTYLE,
            (state.original_ex_style & !WS_EX_APPWINDOW) as isize,
        )?;
        state.validate()?;
        SetLastError(0);
        let previous_parent = SetParent(state.target.hwnd, self.hwnd);
        let error = GetLastError();
        if previous_parent.is_null() && error != 0 {
            return Err(error_with_code(
                tr(
                    "无法将目标窗口放入透明容器",
                    "Cannot embed the target window in its transparency container",
                ),
                error,
            ));
        }
        if GetParent(state.target.hwnd) != self.hwnd {
            return Err(tr(
                "目标窗口未进入透明容器，已取消嵌入",
                "The target window did not enter its transparency container; embedding was cancelled",
            )
            .into());
        }
        state.validate()?;
        if SetWindowPos(
            state.target.hwnd,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
        ) == 0
        {
            return Err(last_error(tr(
                "无法更新嵌入窗口边框",
                "Cannot update the hosted window frame",
            )));
        }
        // 自定义工作区和边距就绪后再承接原最大化状态，避免创建阶段套用系统厚边框
        if state.original_style & WS_MAXIMIZE != 0 {
            self.mirror_target_maximized(true)?;
        }
        self.sync()
    }

    pub fn hwnd(&self) -> Result<HWND, String> {
        unsafe { self.state()? };
        Ok(self.hwnd)
    }

    pub fn is_topmost(&self) -> Result<bool, String> {
        unsafe {
            let state = self.state()?;
            state.validate()?;
            if state.restoring.get() || !state.attached_to(self.hwnd) {
                return Err(tr(
                    "容器状态已变化，请先取消独立容器后重试",
                    "The container state changed; exit the independent container and retry",
                )
                .into());
            }
            let host_topmost = window_is_topmost(self.hwnd)?;
            let target_topmost =
                state.mode == HostingMode::TopLevel && window_is_topmost(state.target.hwnd)?;
            state.validate_host(self.hwnd)?;
            state.validate()?;
            Ok(host_topmost || target_topmost)
        }
    }

    pub fn has_pending_topmost(&self) -> bool {
        window_topmost::is_pending(self.hwnd)
            || window_topmost::is_pending(self.owned_state.target.hwnd)
    }

    pub fn set_topmost(&self, enabled: bool) -> Result<bool, String> {
        unsafe {
            let state = self.state()?;
            state.validate()?;
            if state.restoring.get() || !state.attached_to(self.hwnd) {
                return Err(tr(
                    "容器状态已变化，请先取消独立容器后重试",
                    "The container state changed; exit the independent container and retry",
                )
                .into());
            }
            if !state.original_owner.is_null() {
                return Err(tr(
                    "原窗口具有外部所属关系，无法保证独立恢复置顶状态，请选择所属窗口操作",
                    "The original window has an external owner; select that owner so the topmost state can be restored independently",
                ).into());
            }
            // 顶层目标随所有者进入同一置顶分组，恢复时还原原窗口的分组
            state.topmost_modified.set(true);
            let allowed = (state.mode == HostingMode::TopLevel).then_some(state.target.hwnd);
            if !window_topmost::request(
                self.hwnd,
                enabled,
                allowed,
                window_topmost::Authority::new(&state.target, &state.property, state.token),
            )? {
                return Ok(false);
            }
            state.validate_host(self.hwnd)?;
            state.validate()?;
            // 原应用可能独立改变自身置顶标志，所有者联动后再核对内容窗口
            if state.mode == HostingMode::TopLevel
                && window_is_topmost(state.target.hwnd)? != enabled
            {
                if !window_topmost::request(
                    state.target.hwnd,
                    enabled,
                    Some(self.hwnd),
                    window_topmost::Authority::new(&state.target, &state.property, state.token),
                )? {
                    return Ok(false);
                }
                state.validate_host(self.hwnd)?;
                state.validate()?;
            }
            Ok(true)
        }
    }

    pub fn set_transparency(&mut self, transparency: u8) -> Result<(), String> {
        unsafe {
            let state = self.state()?;
            state.validate()?;
            let percent = transparency.min(100);
            let alpha = ((100 - percent as u32) * 255 + 50) / 100;
            if state.mode == HostingMode::TopLevel {
                let previous = state.transparency.get();
                if let Err(error) = state.apply_compatibility_alpha(percent) {
                    let _ = state.apply_compatibility_alpha(previous);
                    return Err(error);
                }
                state.validate_host(self.hwnd)?;
                state.validate()?;
            }
            let (color_key, flags) = if state.mode == HostingMode::TopLevel {
                (COMPATIBILITY_COLOR_KEY, LWA_ALPHA | LWA_COLORKEY)
            } else {
                (0, LWA_ALPHA)
            };
            if SetLayeredWindowAttributes(self.hwnd, color_key, alpha as u8, flags) == 0 {
                let error = last_error(tr(
                    "无法设置容器透明度",
                    "Cannot set the container transparency",
                ));
                if state.mode == HostingMode::TopLevel {
                    let _ = state.apply_compatibility_alpha(state.transparency.get());
                }
                return Err(error);
            }
            state.transparency.set(percent);
            Ok(())
        }
    }

    pub fn transparency(&self) -> u8 {
        unsafe {
            self.state()
                .map(|state| state.transparency.get())
                .unwrap_or(0)
        }
    }

    pub fn is_attached(&self) -> bool {
        unsafe {
            self.state().is_ok_and(|state| {
                state.needs_restore.get()
                    && state.validate().is_ok()
                    && state.attached_to(self.hwnd)
            })
        }
    }

    pub fn sync(&mut self) -> Result<(), String> {
        unsafe {
            // 后台置顶尚未返回时不再从界面线程向同一目标同步发送布局消息
            if self.has_pending_topmost() {
                return Ok(());
            }
            let state = self.state()?;
            if state.mirroring_target_state.get() || state.resizing.get() {
                return Ok(());
            }
            if state.host_animation_reset_pending.get() {
                // 上次恢复动画策略失败时，在下一次真实事件中重试
                set_host_transitions_disabled(self.hwnd, false)?;
                self.state()?.host_animation_reset_pending.set(false);
            }
            let state = self.state()?;
            state.validate()?;
            let _dpi_scope = DpiScope::enter(state.dpi_context)?;
            if !state.needs_restore.get() || state.restoring.get() {
                return Ok(());
            }
            if !state.attached_to(self.hwnd) {
                return Err(tr(
                    "目标窗口已脱离容器模式，请先恢复此窗口",
                    "The target left container mode; restore this window first",
                )
                .into());
            }
            if state.mode == HostingMode::TopLevel {
                return self.sync_top_level();
            }
            let visible = GetWindowLongPtrW(state.target.hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0;
            let minimized = IsIconic(state.target.hwnd) != 0;
            let maximized = IsZoomed(state.target.hwnd) != 0;
            let was_visible = state.target_was_visible.replace(visible);
            let was_minimized = state.target_was_minimized.replace(minimized);
            let was_maximized = state.target_was_maximized.get();
            let restoring_target = state.target_restore_pending.get();
            if visible && !minimized {
                state.target_restore_pending.set(false);
            }
            let title = window_title(state.target.hwnd);
            if *state.title.borrow() != title {
                state.title.replace(title.clone());
                let title = wide(&title);
                SetWindowTextW(self.hwnd, title.as_ptr());
            }
            let state = self.state()?;
            state.validate()?;
            if !visible {
                if !state.activation_pending.get() && IsWindowVisible(self.hwnd) != 0 {
                    ShowWindow(self.hwnd, SW_HIDE);
                }
            } else if minimized {
                // 等待子窗处理恢复请求时，不能用它的旧状态再次收起宿主
                if !restoring_target && IsIconic(self.hwnd) == 0 {
                    ShowWindow(self.hwnd, SW_SHOWMINNOACTIVE);
                }
            } else {
                if maximized != was_maximized {
                    if (IsZoomed(self.hwnd) != 0) != maximized {
                        self.mirror_target_maximized(maximized)?;
                    }
                    let state = self.state()?;
                    state.validate()?;
                    state.target_was_maximized.set(maximized);
                }
                let state = self.state()?;
                state.validate()?;
                let target_reopened = (!was_visible || was_minimized) && !restoring_target;
                if target_reopened {
                    state.layout_request.set(None);
                }
                if IsWindowVisible(self.hwnd) == 0 || (IsIconic(self.hwnd) != 0 && target_reopened)
                {
                    ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                let state = self.state()?;
                state.validate()?;
                if state.activation_pending.get() {
                    state.finish_activation(self.hwnd)?;
                } else {
                    state.resize_target(self.hwnd)?;
                }
            }
            Ok(())
        }
    }

    unsafe fn sync_top_level(&self) -> Result<(), String> {
        let state = self.state()?;
        state.validate()?;
        let visible = GetWindowLongPtrW(state.target.hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0;
        let minimized = IsIconic(state.target.hwnd) != 0;
        let was_minimized = state.target_was_minimized.replace(minimized);
        state.target_was_visible.set(visible);
        // 所有者最小化会自动隐藏关联窗口，不能把这一隐藏反向同步为关闭宿主
        if IsIconic(self.hwnd) != 0 {
            if visible && !minimized && was_minimized {
                state.compatibility_layout_pending.set(true);
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            } else {
                return Ok(());
            }
        }
        let title = window_title(state.target.hwnd);
        if *state.title.borrow() != title {
            state.title.replace(title.clone());
            SetWindowTextW(self.hwnd, wide(&title).as_ptr());
        }
        let state = self.state()?;
        state.validate()?;
        if !visible {
            if IsWindowVisible(self.hwnd) != 0 && !state.activation_pending.get() {
                ShowWindow(self.hwnd, SW_HIDE);
            }
            return Ok(());
        }
        if minimized {
            state.compatibility_layout_pending.set(true);
            if !state.target_restore_pending.get() {
                ShowWindow(self.hwnd, SW_SHOWMINNOACTIVE);
            }
            return Ok(());
        }
        state.target_restore_pending.set(false);
        state.apply_compatibility_alpha(state.transparency.get())?;
        state.suppress_target_shadow()?;
        state.suppress_target_corners()?;
        state.suppress_target_frame()?;
        let state = self.state()?;
        state.validate()?;
        if IsZoomed(state.target.hwnd) != 0 {
            // 顶层窗口的最大化会覆盖外框，转由外框承接最大化并异步恢复内容窗口
            if !state.compatibility_restore_pending.get() {
                state.compatibility_layout_pending.set(true);
                self.mirror_target_maximized(true)?;
                let state = self.state()?;
                state.validate()?;
                state.compatibility_restore_pending.set(true);
                if ShowWindowAsync(state.target.hwnd, SW_RESTORE) == 0 {
                    state.compatibility_restore_pending.set(false);
                    return Err(last_error(tr(
                        "无法恢复兼容窗口布局",
                        "Cannot restore the compatibility window layout",
                    )));
                }
            }
            return Ok(());
        }
        if state.compatibility_restore_pending.replace(false) {
            state.compatibility_layout_pending.set(true);
        }
        if IsWindowVisible(self.hwnd) == 0 {
            state.compatibility_layout_pending.set(true);
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        let state = self.state()?;
        state.validate()?;
        if state.compatibility_layout_pending.get() || IsZoomed(self.hwnd) != 0 {
            state.layout_request.set(None);
            state.resize_target(self.hwnd)?;
        } else {
            state.follow_top_level(self.hwnd)?;
        }
        state.finish_activation(self.hwnd)
    }

    unsafe fn mirror_target_maximized(&self, maximized: bool) -> Result<(), String> {
        // 目标已执行状态转换，宿主只跟随状态，不再播放第二段动画
        let scope = HostAnimationScope::begin(self)?;
        let mirrored: Result<(), String> = (|| {
            self.state()?.validate()?;
            ShowWindow(self.hwnd, if maximized { SW_MAXIMIZE } else { SW_RESTORE });
            let state = self.state()?;
            state.validate()?;
            if (IsZoomed(self.hwnd) != 0) != maximized || IsIconic(self.hwnd) != 0 {
                return Err(tr(
                    "容器未能完成窗口状态同步",
                    "The container could not synchronize the window state",
                )
                .into());
            }
            // WM_SIZE 在镜像中不改子窗，策略恢复后再做最终布局
            state.layout_request.set(None);
            Ok(())
        })();
        match (mirrored, scope.finish()) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(reset_error)) => Err(format!("{error}\n{reset_error}")),
        }
    }

    pub fn activate(&self) -> Result<bool, String> {
        unsafe {
            let state = self.state()?;
            state.validate()?;
            let was_pending = state.activation_pending.replace(true);
            // 显示请求与最终布局分开处理，事件确认子窗恢复后继续完成激活
            if let Err(error) = state.restore_target(true) {
                state.activation_pending.set(was_pending);
                return Err(error);
            }
            if IsIconic(self.hwnd) != 0 {
                ShowWindow(self.hwnd, SW_RESTORE);
            } else if IsWindowVisible(self.hwnd) == 0 {
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            let state = self.state()?;
            state.validate()?;
            let foreground = SetForegroundWindow(self.hwnd) != 0;
            let state = self.state()?;
            state.finish_activation(self.hwnd)?;
            Ok(foreground)
        }
    }

    pub fn detach(&mut self) -> Result<(), String> {
        unsafe {
            if self.hwnd.is_null() {
                return Ok(());
            }
            if self.has_pending_topmost() {
                return Err(window_topmost::pending_message());
            }
            let host = self.hwnd;
            // 宿主异常关闭后目标可能已自行脱离，仍需依靠保留的快照恢复样式与位置
            let state: &ContainerState = if IsWindow(host) != 0 {
                self.state()?
            } else {
                &self.owned_state
            };
            if state.target_identity_gone() {
                if !GetWindow(host, GW_CHILD).is_null() {
                    return Err(tr("容器仍有子窗口，无法安全释放，请保留 Trans", "The container still has a child window and cannot be released safely; keep Trans running").into());
                }
                return self.destroy_empty_host();
            }
            if state.restoring.replace(true) {
                return Err(tr(
                    "窗口正在恢复，请稍后重试",
                    "The window is being restored; try again shortly",
                )
                .into());
            }
            let restore = (|| {
                state.validate()?;
                let _dpi_scope = DpiScope::enter(state.dpi_context)?;
                if state.needs_restore.get() {
                    if state.mode == HostingMode::Embedded
                        && (GetAncestor(state.target.hwnd, GA_PARENT) == host
                            || GetParent(state.target.hwnd) == host)
                    {
                        state.validate()?;
                        SetLastError(0);
                        let previous_parent = SetParent(state.target.hwnd, null_mut());
                        let error = GetLastError();
                        if previous_parent.is_null() && error != 0 {
                            return Err(error_with_code(
                                tr(
                                    "无法将窗口从透明容器还原，请保留 Trans 并重试",
                                    "Cannot detach the hosted window; keep Trans running and retry",
                                ),
                                error,
                            ));
                        }
                        if GetAncestor(state.target.hwnd, GA_PARENT) == host {
                            return Err(tr(
                                "目标窗口仍属于透明容器，请保留 Trans 并重试",
                                "The target window still belongs to the container; keep Trans running and retry",
                            ).into());
                        }
                    }
                    state.set_target_long(GWL_STYLE, state.original_style as isize)?;
                    // 置顶分组由异步请求恢复，不先伪写样式位以免误判已经完成
                    let topmost = read_extended_style(state.target.hwnd)? & WS_EX_TOPMOST;
                    state.set_target_long(
                        GWL_EXSTYLE,
                        ((state.original_ex_style & !WS_EX_TOPMOST) | topmost) as isize,
                    )?;
                    let owner = if IsWindow(state.original_owner) != 0 {
                        state.original_owner
                    } else {
                        null_mut()
                    };
                    state.set_target_long(GWLP_HWNDPARENT, owner as isize)?;
                    if state.mode == HostingMode::TopLevel {
                        state.validate()?;
                        if GetWindow(state.target.hwnd, GW_OWNER) == host {
                            return Err(tr("目标窗口仍关联兼容容器，请保留 Trans 并重试", "The target window still belongs to the compatibility container; keep Trans running and retry").into());
                        }
                        if let Some(layered) = state.original_layered {
                            if SetLayeredWindowAttributes(
                                state.target.hwnd,
                                layered.color_key,
                                layered.alpha,
                                layered.flags,
                            ) == 0
                            {
                                return Err(last_error(tr(
                                    "无法还原窗口原始分层属性",
                                    "Cannot restore the original layered attributes",
                                )));
                            }
                        }
                        state.restore_target_nc_rendering()?;
                        state.restore_target_corner_preference()?;
                    }
                    state.validate()?;
                    if SetWindowPos(
                        state.target.hwnd,
                        null_mut(),
                        state.original_rect.left,
                        state.original_rect.top,
                        (state.original_rect.right - state.original_rect.left).max(1),
                        (state.original_rect.bottom - state.original_rect.top).max(1),
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                    ) == 0
                    {
                        return Err(last_error(tr(
                            "无法还原窗口边框与位置",
                            "Cannot restore the window frame and placement",
                        )));
                    }
                    state.validate()?;
                    if state.topmost_modified.get() {
                        if !restore_window_topmost(
                            state.target.hwnd,
                            state.original_ex_style & WS_EX_TOPMOST != 0,
                            &state.target,
                            &state.property,
                            state.token,
                        )? {
                            return Err(window_topmost::pending_message());
                        }
                        state.validate()?;
                        state.topmost_modified.set(false);
                    }
                    if SetWindowPlacement(state.target.hwnd, &state.original_placement) == 0 {
                        return Err(last_error(tr(
                            "无法还原窗口显示状态",
                            "Cannot restore the window display state",
                        )));
                    }
                    state.needs_restore.set(false);
                }
                Ok(())
            })();
            state.restoring.set(false);
            if let Err(error) = restore {
                if state.target_identity_gone() {
                    return self
                        .destroy_empty_host()
                        .map_err(|cleanup_error| format!("{error}\n{cleanup_error}"));
                }
                return Err(error);
            }
            self.destroy_empty_host()
        }
    }

    unsafe fn state(&self) -> Result<&ContainerState, String> {
        if self.hwnd.is_null() || IsWindow(self.hwnd) == 0 {
            return Err(tr("透明容器已关闭", "The transparency container has closed").into());
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(self.hwnd, &mut process_id);
        let ptr = GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *const ContainerState;
        if process_id != GetCurrentProcessId()
            || thread_id != self.thread_id
            || GetCurrentThreadId() != self.thread_id
            || GetPropW(self.hwnd, host_property().as_ptr()) as usize != self.nonce
            || ptr != self.state_ptr
            || GetWindowLongPtrW(self.hwnd, GWLP_WNDPROC) != container_proc as *const () as isize
        {
            return Err(tr(
                "透明容器身份已变化，无法安全恢复，请保留 Trans",
                "The transparency container identity changed; keep Trans running because it cannot be restored safely",
            )
            .into());
        }
        let state = ptr.as_ref().ok_or_else(|| {
            tr(
                "透明容器状态不可用",
                "The transparency container state is unavailable",
            )
            .to_string()
        })?;
        if state.host_nonce != self.nonce {
            return Err(tr(
                "透明容器实例已失效",
                "The transparency container instance is no longer valid",
            )
            .into());
        }
        Ok(state)
    }

    unsafe fn destroy_empty_host(&mut self) -> Result<(), String> {
        if IsWindow(self.hwnd) == 0 {
            self.hwnd = null_mut();
            return Ok(());
        }
        self.state()?;
        let state = &self.owned_state;
        if IsWindow(state.target.hwnd) != 0
            && (GetParent(state.target.hwnd) == self.hwnd
                || GetWindow(state.target.hwnd, GW_OWNER) == self.hwnd
                || GetAncestor(state.target.hwnd, GA_PARENT) == self.hwnd)
        {
            return Err(tr("目标窗口仍关联容器，无法安全释放，请保留 Trans", "The target window still belongs to the container; keep Trans running because it cannot be released safely").into());
        }
        if !GetWindow(self.hwnd, GW_CHILD).is_null() {
            return Err(tr("容器仍有子窗口，无法安全释放，请保留 Trans", "The container still has a child window and cannot be released safely; keep Trans running").into());
        }
        if DestroyWindow(self.hwnd) == 0 {
            return Err(last_error(tr(
                "无法释放透明容器",
                "Cannot release the transparency container",
            )));
        }
        self.hwnd = null_mut();
        Ok(())
    }
}

impl Drop for WindowContainer {
    fn drop(&mut self) {
        // 还原失败时由窗口自身继续持有状态，不能连带销毁用户窗口
        let _ = self.detach();
    }
}

unsafe extern "system" fn container_proc(
    host: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 标题栏、边框命中及系统移动缩放统一交给 Windows 原生处理
    let ptr = GetWindowLongPtrW(host, GWLP_USERDATA) as *const ContainerState;
    // 每次回调单独保活，嵌套 WM_NCDESTROY 只释放宿主持有的引用
    let state = if ptr.is_null() {
        None
    } else {
        Rc::increment_strong_count(ptr);
        Some(Rc::from_raw(ptr))
    };
    if message == WM_DESTROY {
        // 先结束 RefCell 借用，再清理可能引发 COM 重入的窗口属性
        let group = state
            .as_deref()
            .and_then(|state| state.taskbar_group.borrow_mut().take());
        drop(group);
        return DefWindowProcW(host, message, wparam, lparam);
    }
    if message == WM_NCDESTROY {
        SetWindowLongPtrW(host, GWLP_USERDATA, 0);
        RemovePropW(host, host_property().as_ptr());
        let host_state = (!ptr.is_null()).then(|| Rc::from_raw(ptr));
        let group = state
            .as_deref()
            .and_then(|state| state.taskbar_group.borrow_mut().take());
        drop(group);
        let result = DefWindowProcW(host, message, wparam, lparam);
        drop(host_state);
        return result;
    }
    if let Some(state) = state.as_deref() {
        if let Some(result) = state.caption.process(host, message, wparam, lparam) {
            if state.mode == HostingMode::TopLevel
                && matches!(message, WM_LBUTTONUP | WM_CANCELMODE | WM_CAPTURECHANGED)
            {
                // 按钮系统命令先入队，鼠标操作结束后再交还原窗口输入
                PostMessageW(host, WM_COMPATIBILITY_FOCUS, 0, 0);
            }
            return result;
        }
        match message {
            WM_ERASEBKGND if state.mode == HostingMode::TopLevel => {
                return fill_compatibility_client(host, wparam as HDC) as LRESULT;
            }
            WM_PAINT if state.mode == HostingMode::TopLevel => {
                let mut paint: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(host, &mut paint);
                if !dc.is_null() {
                    fill_compatibility_client(host, dc);
                }
                EndPaint(host, &paint);
                return 0;
            }
            WM_CLOSE => {
                state.activation_pending.set(false);
                state.target_restore_pending.set(false);
                if state.validate().is_ok() {
                    PostMessageW(state.target.hwnd, WM_CLOSE, 0, 0);
                }
                return 0;
            }
            WM_DPICHANGED if lparam != 0 && !state.restoring.get() => {
                state.layout_request.set(None);
                let bounds = *(lparam as *const RECT);
                SetWindowPos(
                    host,
                    null_mut(),
                    bounds.left,
                    bounds.top,
                    (bounds.right - bounds.left).max(1),
                    (bounds.bottom - bounds.top).max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                return 0;
            }
            WM_SIZE
                if !state.restoring.get()
                    && !state.mirroring_target_state.get()
                    && state.needs_restore.get()
                    && state.validate().is_ok()
                    && state.attached_to(host) =>
            {
                if state.mode == HostingMode::TopLevel {
                    state.compatibility_layout_pending.set(true);
                }
                RedrawWindow(host, null_mut(), null_mut(), RDW_INVALIDATE | RDW_FRAME);
                // 宿主负责收起动画，不再异步收起子窗或伪造它的历史状态
                if wparam == SIZE_MINIMIZED as usize {
                    state.activation_pending.set(false);
                } else if matches!(wparam as u32, SIZE_RESTORED | SIZE_MAXIMIZED) {
                    state.layout_request.set(None);
                    if IsWindowVisible(host) != 0 && IsIconic(state.target.hwnd) != 0 {
                        let was_pending = state.activation_pending.replace(true);
                        if state.restore_target(false).is_err() {
                            state.activation_pending.set(was_pending);
                        }
                    }
                    if state.activation_pending.get() {
                        let _ = state.finish_activation(host);
                    } else {
                        let _ = state.resize_target(host);
                    }
                }
            }
            WM_MOVE
                if state.mode == HostingMode::TopLevel
                    && !state.restoring.get()
                    && !state.mirroring_target_state.get()
                    && !state.resizing.get()
                    && state.needs_restore.get()
                    && state.validate().is_ok()
                    && state.attached_to(host) =>
            {
                state.compatibility_layout_pending.set(true);
                let _ = state.resize_target(host);
            }
            WM_EXITSIZEMOVE | WM_EXITMENULOOP if state.mode == HostingMode::TopLevel => {
                PostMessageW(host, WM_COMPATIBILITY_FOCUS, 0, 0);
            }
            WM_COMPATIBILITY_FOCUS if state.mode == HostingMode::TopLevel => {
                if !state.restoring.get()
                    && state.validate().is_ok()
                    && state.attached_to(host)
                    && GetForegroundWindow() == host
                    && GetCapture() != host
                    && IsWindowVisible(state.target.hwnd) != 0
                    && IsIconic(state.target.hwnd) == 0
                {
                    let mut info: GUITHREADINFO = zeroed();
                    info.cbSize = size_of::<GUITHREADINFO>() as u32;
                    // 不在原生拖动和系统菜单循环中切走前台，结束拖动后再交还输入
                    if GetGUIThreadInfo(GetCurrentThreadId(), &mut info) != 0
                        && info.flags & (GUI_INMOVESIZE | GUI_INMENUMODE | GUI_SYSTEMMENUMODE) == 0
                    {
                        SetForegroundWindow(state.target.hwnd);
                    }
                }
                return 0;
            }
            WM_SETFOCUS if !state.restoring.get() && state.validate().is_ok() => {
                if state.mode == HostingMode::TopLevel {
                    PostMessageW(host, WM_COMPATIBILITY_FOCUS, 0, 0);
                } else if IsIconic(state.target.hwnd) == 0
                    && IsWindowVisible(state.target.hwnd) != 0
                {
                    SetFocus(state.target.hwnd);
                }
                return 0;
            }
            _ => {}
        }
    }
    DefWindowProcW(host, message, wparam, lparam)
}

unsafe fn read_nc_rendering(target: HWND) -> Result<bool, String> {
    let mut enabled = 0i32;
    let result = DwmGetWindowAttribute(
        target,
        DWMWA_NCRENDERING_ENABLED as u32,
        &mut enabled as *mut i32 as _,
        size_of_val(&enabled) as u32,
    );
    if result < 0 {
        return Err(format!(
            "{} (HRESULT 0x{:08X})",
            tr(
                "无法读取窗口的非客户区绘制状态",
                "Cannot read the window's non-client rendering state"
            ),
            result as u32,
        ));
    }
    Ok(enabled != 0)
}

unsafe fn read_corner_preference(target: HWND) -> Option<i32> {
    let mut preference = 0i32;
    let result = DwmGetWindowAttribute(
        target,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &mut preference as *mut i32 as _,
        size_of_val(&preference) as u32,
    );
    (result >= 0).then_some(preference)
}

unsafe fn fill_compatibility_client(host: HWND, dc: HDC) -> bool {
    if dc.is_null() {
        return false;
    }
    let mut client: RECT = zeroed();
    if GetClientRect(host, &mut client) == 0 {
        return false;
    }
    let brush = GetStockObject(DC_BRUSH);
    if brush.is_null() {
        return false;
    }
    let previous = SetDCBrushColor(dc, COMPATIBILITY_COLOR_KEY);
    if previous == CLR_INVALID {
        return false;
    }
    // 客户区始终透明留空，不在原窗口下面重复铺设带 alpha 的色带
    let filled = FillRect(dc, &client, brush) != 0;
    SetDCBrushColor(dc, previous);
    filled
}

unsafe fn host_bounds(host: HWND) -> Result<(RECT, RECT), String> {
    let mut window: RECT = zeroed();
    let mut client: RECT = zeroed();
    if GetWindowRect(host, &mut window) == 0 || GetClientRect(host, &mut client) == 0 {
        return Err(last_error(tr(
            "无法读取兼容容器边界",
            "Cannot read the compatibility container bounds",
        )));
    }
    SetLastError(0);
    if MapWindowPoints(host, null_mut(), &mut client as *mut RECT as *mut POINT, 2) == 0
        && GetLastError() != 0
    {
        return Err(last_error(tr(
            "无法换算兼容容器坐标",
            "Cannot map the compatibility container coordinates",
        )));
    }
    Ok((window, client))
}

unsafe fn read_window_class(hwnd: HWND) -> Result<String, String> {
    let mut class = [0u16; 256];
    let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    if length == 0 {
        return Err(last_error(tr(
            "无法检查窗口类别，已保留原状态",
            "Cannot check the window class; the window was left unchanged",
        )));
    }
    Ok(String::from_utf16_lossy(&class[..length as usize]))
}

unsafe fn read_extended_style(hwnd: HWND) -> Result<u32, String> {
    SetLastError(0);
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let error = GetLastError();
    if style == 0 && error != 0 {
        return Err(error_with_code(
            tr(
                "无法检查窗口绘制样式，已保留原状态",
                "Cannot check the rendering style; the window was left unchanged",
            ),
            error,
        ));
    }
    Ok(style as u32)
}

unsafe fn set_window_long(hwnd: HWND, index: i32, value: isize) -> Result<(), String> {
    SetLastError(0);
    let current = GetWindowLongPtrW(hwnd, index);
    let read_error = GetLastError();
    if current == 0 && read_error != 0 {
        return Err(error_with_code(
            tr(
                "无法读取窗口样式或所属窗口",
                "Cannot read the window style or owner",
            ),
            read_error,
        ));
    }
    let unchanged = if index == GWL_STYLE || index == GWL_EXSTYLE {
        current as u32 == value as u32
    } else {
        current == value
    };
    if unchanged {
        return Ok(());
    }
    SetLastError(0);
    let previous = SetWindowLongPtrW(hwnd, index, value);
    let error = GetLastError();
    if previous == 0 && error != 0 {
        return Err(error_with_code(
            tr("无法修改窗口样式", "Cannot update the window style"),
            error,
        ));
    }
    Ok(())
}

unsafe fn register_class() -> Result<(), String> {
    let result = CONTAINER_CLASS.get_or_init(|| {
        let name = class_name();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(container_proc),
            hInstance: GetModuleHandleW(null_mut()),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW + 1) as _,
            lpszClassName: name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassExW(&class) == 0 {
            Err(GetLastError())
        } else {
            Ok(())
        }
    });
    match result {
        Ok(()) => Ok(()),
        Err(code) => Err(format!(
            "{}: {}",
            tr(
                "无法注册透明容器窗口",
                "Cannot register the transparency container window"
            ),
            std::io::Error::from_raw_os_error(*code as i32)
        )),
    }
}

unsafe fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 1024];
    let count = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
    String::from_utf16_lossy(&buffer[..count.max(0) as usize])
}

fn class_name() -> Vec<u16> {
    wide("Trans.TransparencyContainer.1")
}

fn host_property() -> &'static [u16] {
    HOST_PROPERTY.get_or_init(|| wide("Trans.Container.Instance"))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn last_error(context: &str) -> String {
    let error = GetLastError();
    error_with_code(context, error)
}

fn error_with_code(context: &str, error: u32) -> String {
    format!(
        "{context}: {}",
        std::io::Error::from_raw_os_error(error as i32)
    )
}
