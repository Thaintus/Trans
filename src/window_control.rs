use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_PRIVILEGE_NOT_HELD, HANDLE, HWND, LPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetDesktopWindow, GetLayeredWindowAttributes,
    GetPropW, GetShellWindow, GetWindow, GetWindowLongPtrW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, RemovePropW,
    SetForegroundWindow, SetLayeredWindowAttributes, SetPropW, SetWindowLongPtrW, ShowWindowAsync,
    GA_ROOT, GWL_EXSTYLE, GW_OWNER, LWA_ALPHA, SW_RESTORE, SW_SHOW, WS_EX_APPWINDOW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::localization::tr;
use crate::taskbar::TaskbarList;
use crate::window_container::WindowContainer;
use crate::window_topmost;

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: HWND,
    pub title: String,
    pub process_id: u32,
    pub thread_id: u32,
}

#[derive(Clone, Debug)]
pub struct ChangedWindowInfo {
    pub window: WindowInfo,
    pub token: usize,
    pub transparency: u8,
    // 表示 Trans 成功请求隐藏，不是对 Explorer 当前按钮状态的查询结果
    pub taskbar_hidden: bool,
    pub container_mode: bool,
    pub topmost: bool,
}

#[derive(Clone, Copy)]
struct LayeredAttributes {
    color_key: u32,
    alpha: u8,
    flags: u32,
}

#[derive(Clone, Copy)]
struct Snapshot {
    layered: bool,
    attributes: Option<LayeredAttributes>,
}

#[derive(Clone)]
struct ChangedWindow {
    window: WindowInfo,
    original: Snapshot,
    token: usize,
    last_transparency: u8,
    transparency_modified: bool,
    desired_transparency: Option<u8>,
    maintenance_errors: HashSet<String>,
    maintenance_failures: u8,
    next_maintenance_retry: Option<Instant>,
    taskbar_hidden: bool,
    taskbar_maintenance_enabled: bool,
    original_taskbar_visible: Option<bool>,
    original_topmost: Option<bool>,
    desired_topmost: Option<bool>,
}

impl ChangedWindow {
    fn maintenance_is_cooling_down(&self, now: Instant) -> bool {
        self.next_maintenance_retry.is_some_and(|retry| now < retry)
    }

    fn clear_maintenance_failures(&mut self) {
        self.maintenance_errors.clear();
        self.maintenance_failures = 0;
        self.next_maintenance_retry = None;
    }
}

struct SelectedWindow {
    window: WindowInfo,
    token: usize,
}

#[derive(Clone, Copy)]
enum TopmostAction {
    Set(bool),
    DisableContainer,
    Restore,
}

#[derive(Clone, Copy)]
struct PendingTopmostAction {
    token: usize,
    action: TopmostAction,
}

pub struct WindowController {
    changed: HashMap<usize, ChangedWindow>,
    containers: HashMap<usize, WindowContainer>,
    container_errors: HashMap<usize, String>,
    container_restores: HashSet<usize>,
    topmost_actions: HashMap<usize, PendingTopmostAction>,
    property_name: Vec<u16>,
    selection: Option<SelectedWindow>,
    selection_property_name: Vec<u16>,
}

impl WindowController {
    pub fn new() -> Self {
        let instance = next_id();
        let process_id = unsafe { GetCurrentProcessId() };
        let property_name = format!("Trans.Owner.{process_id}.{instance}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let selection_property_name = format!("Trans.Selection.{process_id}.{instance}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        Self {
            changed: HashMap::new(),
            containers: HashMap::new(),
            container_errors: HashMap::new(),
            container_restores: HashSet::new(),
            topmost_actions: HashMap::new(),
            property_name,
            selection: None,
            selection_property_name,
        }
    }

    pub fn watch_selection(&mut self, window: &WindowInfo) -> Result<(), String> {
        if let Some(selection) = self.selection.as_ref() {
            if same_identity(&selection.window, window) && self.owns_selection(selection) {
                self.validate_managed_target(window)?;
                return Ok(());
            }
        }
        self.clear_selection();
        self.validate_managed_target(window)?;
        let token = next_id();
        unsafe {
            SetLastError(0);
            if SetPropW(
                window.hwnd,
                self.selection_property_name.as_ptr(),
                token as HANDLE,
            ) == 0
            {
                return Err(api_error(
                    tr("无法标记选中窗口", "Cannot mark the selected window"),
                    GetLastError(),
                ));
            }
        }
        let selection = SelectedWindow {
            window: window.clone(),
            token,
        };
        if !self.owns_selection(&selection) {
            return Err(tr(
                "选中的窗口已关闭或发生变化，请重新选择",
                "The selected window has closed or changed; select it again",
            )
            .into());
        }
        self.selection = Some(selection);
        Ok(())
    }

    pub fn validate_selection(&self, window: &WindowInfo) -> Result<(), String> {
        self.require_selection(window)?;
        self.validate_managed_target(window)
    }

    pub fn clear_selection(&mut self) {
        if let Some(selection) = self.selection.take() {
            if self.owns_selection(&selection) {
                unsafe {
                    RemovePropW(selection.window.hwnd, self.selection_property_name.as_ptr());
                }
            }
        }
    }

    pub fn track(&mut self, window: &WindowInfo) -> Result<(), String> {
        self.require_selection(window)?;
        self.validate_managed_target(window)?;
        let key = window.hwnd as usize;

        if let Some(previous) = self.changed.get(&key) {
            if !self.owns(previous) {
                self.forget_stale(key);
                return Err(tr("原窗口已关闭或窗口标记已失效，请刷新列表后重新选择", "The original window has closed or its marker is invalid; refresh the list and select again").into());
            }
            if !same_identity(&previous.window, window) {
                return Err(tr(
                    "选中的窗口身份已变化，请刷新列表后重新选择",
                    "The selected window identity changed; refresh the list and select again",
                )
                .into());
            }
            return Ok(());
        }

        let token = next_id();
        self.require_selection(window)?;
        unsafe {
            SetLastError(0);
            if SetPropW(window.hwnd, self.property_name.as_ptr(), token as HANDLE) == 0 {
                return Err(api_error(
                    tr("无法标记目标窗口", "Cannot mark the target window"),
                    GetLastError(),
                ));
            }
        }

        let temporary = ChangedWindow {
            window: window.clone(),
            original: Snapshot {
                layered: false,
                attributes: None,
            },
            token,
            last_transparency: 0,
            transparency_modified: false,
            desired_transparency: None,
            maintenance_errors: HashSet::new(),
            maintenance_failures: 0,
            next_maintenance_retry: None,
            taskbar_hidden: false,
            taskbar_maintenance_enabled: false,
            original_taskbar_visible: None,
            original_topmost: None,
            desired_topmost: None,
        };
        if !self.owns(&temporary) {
            self.remove_marker_if_owned(&temporary);
            return Err(tr(
                "目标窗口已关闭或发生变化，请重新选择",
                "The target window has closed or changed; select it again",
            )
            .into());
        }
        if let Err(error) = self.require_selection(window) {
            self.remove_marker_if_owned(&temporary);
            return Err(error);
        }
        let original = match snapshot(window.hwnd) {
            Ok(original) => original,
            Err(error) => {
                self.remove_marker_if_owned(&temporary);
                return Err(error);
            }
        };
        if !self.owns(&temporary) {
            self.remove_marker_if_owned(&temporary);
            return Err(tr(
                "目标窗口已关闭或发生变化，请重新选择",
                "The target window has closed or changed; select it again",
            )
            .into());
        }
        if let Err(error) = self.require_selection(window) {
            self.remove_marker_if_owned(&temporary);
            return Err(error);
        }
        self.changed.insert(
            key,
            ChangedWindow {
                original,
                last_transparency: snapshot_transparency(original),
                ..temporary
            },
        );
        Ok(())
    }

    pub fn apply(&mut self, window: &WindowInfo, transparency: u8) -> Result<(), String> {
        if transparency > 100 {
            return Err(tr(
                "透明程度必须在 0 到 100 之间",
                "Transparency must be between 0 and 100",
            )
            .into());
        }
        self.track(window)?;
        let key = window.hwnd as usize;
        let tracked = self.changed.get(&key).cloned().expect("已登记窗口");
        if let Some(container) = self.containers.get_mut(&key) {
            container.set_transparency(transparency)?;
            if let Some(current) = self.changed.get_mut(&key) {
                current.last_transparency = transparency;
                current.desired_transparency = Some(transparency);
                current.clear_maintenance_failures();
            }
            return Ok(());
        }
        let before = snapshot(window.hwnd)?;
        let requested = snapshot_with_transparency(before, transparency);

        self.require_selection(window)?;
        if let Err(error) = self.write_snapshot(&tracked, requested) {
            if !self.owns(&tracked) {
                self.changed.remove(&key);
                return Err(format!(
                    "{error}{}",
                    tr(
                        "；原窗口已关闭或窗口标记已失效",
                        "; the original window has closed or its marker is invalid"
                    )
                ));
            }
            match self.write_snapshot(&tracked, before) {
                Ok(()) => {
                    if let Some(current) = self.changed.get_mut(&key) {
                        current.transparency_modified = tracked.transparency_modified;
                    }
                    return Err(error);
                }
                Err(rollback_error) => {
                    return Err(format!(
                        "{error}{}{rollback_error}{}",
                        tr("；回滚失败：", "; rollback failed: "),
                        tr(
                            "。已保留原始状态，可再次点击恢复",
                            ". The original state was retained; click Restore to retry"
                        )
                    ));
                }
            }
        }
        if let Some(tracked) = self.changed.get_mut(&key) {
            tracked.last_transparency = transparency;
            tracked.transparency_modified = true;
            tracked.desired_transparency = Some(transparency);
            tracked.clear_maintenance_failures();
        }
        Ok(())
    }

    pub fn set_container_mode(&mut self, window: &WindowInfo, enabled: bool) -> Result<(), String> {
        let tracked = self.tracked_window(window)?;
        let key = window.hwnd as usize;
        if self.topmost_actions.contains_key(&key) {
            return Err(window_topmost::pending_message());
        }
        if enabled == self.containers.contains_key(&key) {
            return Ok(());
        }
        if !enabled {
            if !self.detach_container(key)? {
                self.defer_topmost(key, TopmostAction::DisableContainer);
                return Ok(());
            }
            if window_identity_gone(window) {
                self.changed.remove(&key);
                return Ok(());
            }
            let transparency_restored: Result<(), String> = (|| {
                self.require_owned(&tracked)?;
                if let Some(percent) = tracked.desired_transparency {
                    let before = snapshot(window.hwnd)?;
                    self.write_snapshot(&tracked, snapshot_with_transparency(before, percent))?;
                    if let Some(current) = self.changed.get_mut(&key) {
                        current.last_transparency = percent;
                        current.clear_maintenance_failures();
                    }
                }
                Ok(())
            })();
            let taskbar_restored: Result<(), String> = (|| {
                if tracked.taskbar_hidden {
                    let taskbar = TaskbarList::new()?;
                    self.require_owned(&tracked)?;
                    taskbar.set_hidden(window.hwnd, true)?;
                    self.require_owned(&tracked)?;
                }
                Ok(())
            })();
            let topmost_restored = if let Some(enabled) = tracked.desired_topmost {
                self.write_topmost(&tracked, enabled).map(|complete| {
                    if !complete {
                        self.defer_topmost(key, TopmostAction::Set(enabled));
                    }
                })
            } else {
                Ok(())
            };
            let errors: Vec<_> = [transparency_restored, taskbar_restored, topmost_restored]
                .into_iter()
                .filter_map(Result::err)
                .collect();
            let restored = if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("\n"))
            };
            if restored.is_err() && window_identity_gone(window) {
                self.changed.remove(&key);
                return Ok(());
            }
            return restored;
        }
        validate_target(window)?;
        // 预检失败不能先撤销直接透明度，保留用户当前状态
        self.require_owned(&tracked)?;
        let compatibility = WindowContainer::check_render_compatibility(window);
        self.require_owned(&tracked)?;
        compatibility?;
        let before = snapshot(window.hwnd)?;
        if snapshot_alpha(tracked.original) != 255
            || (!tracked.transparency_modified && snapshot_alpha(before) != 255)
        {
            return Err(tr("该窗口原本已有透明度，当前容器实验模式仅支持原始不透明的窗口，以避免透明度叠加", "This window already has native transparency; experimental container mode currently requires an originally opaque window to avoid multiplying opacity").into());
        }
        let percent = tracked
            .desired_transparency
            .unwrap_or(tracked.last_transparency);
        let original_taskbar_visible = tracked
            .original_taskbar_visible
            .unwrap_or_else(|| validate_taskbar_candidate(window).is_ok());
        // 先撤销本工具对原窗口的透明度，避免原窗口和宿主两层透明度叠乘
        if tracked.transparency_modified {
            if let Err(error) = self.write_snapshot(&tracked, tracked.original) {
                let _ = self.write_snapshot(&tracked, before);
                return Err(error);
            }
        }
        self.require_owned(&tracked)?;
        match WindowContainer::attach(window, &self.property_name, tracked.token, percent) {
            Ok(container) => {
                self.containers.insert(key, container);
            }
            Err(error) => {
                if let Some(container) = error.container {
                    // 回滚未完成的宿主也登记，确保退出前仍有可重试的脱离入口
                    self.containers.insert(key, container);
                    self.container_restores.insert(key);
                    if let Some(current) = self.changed.get_mut(&key) {
                        current.last_transparency = percent;
                        current.desired_transparency = Some(percent);
                        current.original_taskbar_visible = Some(original_taskbar_visible);
                    }
                    return Err(error.message);
                }
                return match self.write_snapshot(&tracked, before) {
                    Ok(()) => Err(error.message),
                    Err(rollback) => Err(format!(
                        "{}{}{rollback}",
                        error.message,
                        tr("；恢复透明度失败：", "; transparency rollback failed: ")
                    )),
                };
            }
        }
        if let Some(current) = self.changed.get_mut(&key) {
            current.last_transparency = percent;
            current.desired_transparency = Some(percent);
            current.original_taskbar_visible = Some(original_taskbar_visible);
            current.clear_maintenance_failures();
        }
        self.container_errors.remove(&key);
        if let Some(enabled) = tracked.desired_topmost {
            if !self.write_topmost(&tracked, enabled)? {
                self.defer_topmost(key, TopmostAction::Set(enabled));
            }
        }
        if tracked.taskbar_hidden {
            TaskbarList::new()?.set_hidden(self.containers[&key].hwnd()?, true)?;
        }
        Ok(())
    }

    pub fn is_container_mode(&self, window: &WindowInfo) -> bool {
        self.containers.contains_key(&(window.hwnd as usize))
            && self
                .changed
                .get(&(window.hwnd as usize))
                .is_some_and(|tracked| same_identity(&tracked.window, window))
    }

    pub fn can_enable_container(&self, window: &WindowInfo) -> Result<bool, String> {
        let tracked = self.tracked_window(window)?;
        self.require_owned(&tracked)?;
        let supported = WindowContainer::supports_rendering(window);
        self.require_owned(&tracked)?;
        supported
    }

    pub fn transparency_of(&self, window: &WindowInfo) -> Result<u8, String> {
        if let Some(container) = self.containers.get(&(window.hwnd as usize)) {
            self.tracked_window(window)?;
            return Ok(container.transparency());
        }
        transparency_of(window)
    }

    pub fn sync_containers(&mut self) -> Vec<String> {
        let keys: Vec<_> = self.containers.keys().copied().collect();
        keys.into_iter()
            .flat_map(|key| self.sync_container(key))
            .collect()
    }

    fn sync_container(&mut self, key: usize) -> Vec<String> {
        if !self.containers.contains_key(&key) {
            return Vec::new();
        }
        let result = if self
            .changed
            .get(&key)
            .is_some_and(|tracked| !identity_matches(&tracked.window))
        {
            match self.detach_container(key) {
                Ok(true) => {
                    self.changed.remove(&key);
                    return Vec::new();
                }
                Ok(false) => return Vec::new(),
                Err(error) => Err(error),
            }
        } else if self.container_restores.contains(&key) {
            return Vec::new();
        } else {
            self.containers.get_mut(&key).expect("已登记容器").sync()
        };
        match result {
            Ok(()) => {
                self.container_errors.remove(&key);
                Vec::new()
            }
            Err(error) => {
                if self.container_errors.get(&key) == Some(&error) {
                    Vec::new()
                } else {
                    self.container_errors.insert(key, error.clone());
                    vec![error]
                }
            }
        }
    }

    fn detach_container(&mut self, key: usize) -> Result<bool, String> {
        // 一旦开始脱离，失败时也不能继续自动同步旧宿主状态
        self.container_restores.insert(key);
        if let Some(container) = self.containers.get_mut(&key) {
            if let Err(error) = container.detach() {
                if container.has_pending_topmost() {
                    return Ok(false);
                }
                return Err(error);
            }
        }
        self.containers.remove(&key);
        self.container_errors.remove(&key);
        self.container_restores.remove(&key);
        Ok(true)
    }

    fn validate_managed_target(&self, window: &WindowInfo) -> Result<(), String> {
        if let Some(container) = self.containers.get(&(window.hwnd as usize)) {
            self.tracked_window(window)?;
            self.require_container_ready(window.hwnd as usize)?;
            if container.is_attached() {
                return Ok(());
            }
            return Err(tr(
                "容器状态已变化，请先取消独立容器后重试",
                "The container state changed; exit the independent container and retry",
            )
            .into());
        }
        validate_target(window)
    }

    fn require_container_ready(&self, key: usize) -> Result<(), String> {
        if self.container_restores.contains(&key) {
            return Err(tr("此容器尚未完成恢复，请先退出容器或移除该窗口后重试", "This container is awaiting recovery; exit the container or remove the window before retrying").into());
        }
        Ok(())
    }

    fn forget_stale(&mut self, key: usize) {
        // 容器恢复对象只能在安全脱离或原目标销毁后移除
        if !self.containers.contains_key(&key) {
            self.changed.remove(&key);
        }
    }

    fn taskbar_window(&self, tracked: &ChangedWindow) -> Result<HWND, String> {
        self.containers
            .get(&(tracked.window.hwnd as usize))
            .map_or(Ok(tracked.window.hwnd), WindowContainer::hwnd)
    }

    pub fn event_targets(&self) -> Vec<(HWND, usize, bool, bool)> {
        self.changed
            .values()
            .filter(|tracked| {
                self.owns(tracked)
                    || self
                        .containers
                        .contains_key(&(tracked.window.hwnd as usize))
            })
            .map(|tracked| {
                (
                    tracked.window.hwnd,
                    tracked.token,
                    !self
                        .container_restores
                        .contains(&(tracked.window.hwnd as usize))
                        && (self
                            .containers
                            .contains_key(&(tracked.window.hwnd as usize))
                            || tracked.desired_transparency.is_some()
                            || (tracked.taskbar_hidden && tracked.taskbar_maintenance_enabled)),
                    self.containers
                        .contains_key(&(tracked.window.hwnd as usize)),
                )
            })
            .collect()
    }

    pub fn maintain_transparency(&mut self, hwnd: HWND, token: usize) -> Vec<String> {
        let key = hwnd as usize;
        // 容器模式只维护宿主生命周期，不再向原窗口补写透明度
        if self.containers.contains_key(&key) {
            if self
                .changed
                .get(&key)
                .is_some_and(|tracked| tracked.token == token)
            {
                return self.sync_container(key);
            }
            return Vec::new();
        }
        let Some(tracked) = self
            .changed
            .get(&key)
            .filter(|tracked| tracked.token == token && tracked.desired_transparency.is_some())
            .cloned()
        else {
            return Vec::new();
        };
        let mut failures = Vec::new();
        if !self.owns(&tracked) {
            self.changed.remove(&key);
            return failures;
        }
        if !is_visible_restored(hwnd) {
            return failures;
        }
        if tracked.maintenance_is_cooling_down(Instant::now()) {
            return failures;
        }
        if let Err(error) = validate_target(&tracked.window) {
            if self.owns(&tracked) {
                if is_visible_restored(hwnd) {
                    self.record_maintenance_failure(&tracked, error, &mut failures);
                }
            } else {
                self.changed.remove(&key);
            }
            return failures;
        }
        let transparency = tracked.desired_transparency.expect("已设置透明度目标");
        let before = match snapshot(tracked.window.hwnd) {
            Ok(before) => before,
            Err(error) => {
                if self.owns(&tracked) {
                    if is_visible_restored(hwnd) {
                        self.record_maintenance_failure(&tracked, error, &mut failures);
                    }
                } else {
                    self.changed.remove(&key);
                }
                return failures;
            }
        };
        if !self.owns(&tracked) {
            self.changed.remove(&key);
            return failures;
        }
        // 自动维护只处理仍在显示的窗口，隐藏或最小化不改变目标与冷却
        if !is_visible_restored(hwnd) {
            return failures;
        }
        // 只校正用户明确设置过的透明度，不使用列表读数作为维护目标
        if snapshot_alpha(before) == transparency_alpha(transparency) {
            if let Some(current) = self.changed.get_mut(&key) {
                current.last_transparency = snapshot_transparency(before);
                current.clear_maintenance_failures();
            }
            return failures;
        }
        let requested = snapshot_with_transparency(before, transparency);
        if let Err(error) = self.write_snapshot(&tracked, requested) {
            if !self.owns(&tracked) {
                self.changed.remove(&key);
                return failures;
            }
            // 维护失败时回到本次维护前的快照，首次登记的恢复快照保持不变
            let error = match self.write_snapshot(&tracked, before) {
                Ok(()) => {
                    if let Some(current) = self.changed.get_mut(&key) {
                        current.transparency_modified = tracked.transparency_modified;
                    }
                    error
                }
                Err(rollback_error) => format!(
                    "{error}{}{rollback_error}{}",
                    tr("；回滚失败：", "; rollback failed: "),
                    tr(
                        "。已保留原始状态，可再次点击恢复",
                        ". The original state was retained; click Restore to retry"
                    )
                ),
            };
            if self.owns(&tracked) {
                self.record_maintenance_failure(&tracked, error, &mut failures);
            } else {
                self.changed.remove(&key);
            }
            return failures;
        }
        if let Some(current) = self.changed.get_mut(&key) {
            current.last_transparency = transparency;
            current.clear_maintenance_failures();
        }
        failures
    }

    // 返回前台激活是否被 Windows 接受，窗口显示请求可能异步完成
    pub fn activate(&self, window: &WindowInfo) -> Result<bool, String> {
        let tracked = self.tracked_window(window)?;
        self.require_owned(&tracked)?;
        self.require_container_ready(window.hwnd as usize)?;
        if let Some(container) = self.containers.get(&(window.hwnd as usize)) {
            let activated = container.activate()?;
            if tracked.taskbar_hidden && tracked.taskbar_maintenance_enabled {
                TaskbarList::new()?.set_hidden(container.hwnd()?, true)?;
            }
            return Ok(activated);
        }
        unsafe {
            let command = if IsIconic(window.hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            SetLastError(0);
            if ShowWindowAsync(window.hwnd, command) == 0 {
                return Err(api_error(
                    tr("无法显示目标窗口", "Cannot show the target window"),
                    GetLastError(),
                ));
            }
        }
        self.require_owned(&tracked)?;
        let activated = unsafe { SetForegroundWindow(window.hwnd) != 0 };
        if tracked.taskbar_hidden && tracked.taskbar_maintenance_enabled {
            let taskbar = TaskbarList::new()?;
            self.require_owned(&tracked)?;
            taskbar.set_hidden(window.hwnd, true)?;
        }
        self.require_owned(&tracked)?;
        Ok(activated)
    }

    pub fn is_topmost(&self, window: &WindowInfo) -> Result<bool, String> {
        let tracked = self.tracked_window(window)?;
        if self.topmost_actions.contains_key(&(window.hwnd as usize)) {
            return Err(window_topmost::pending_message());
        }
        self.observed_topmost(&tracked)
    }

    fn observed_topmost(&self, tracked: &ChangedWindow) -> Result<bool, String> {
        self.require_owned(tracked)?;
        let window = &tracked.window;
        self.require_container_ready(window.hwnd as usize)?;
        let topmost = if let Some(container) = self.containers.get(&(window.hwnd as usize)) {
            container.is_topmost()?
        } else {
            window_is_topmost(window.hwnd)?
        };
        self.require_owned(tracked)?;
        Ok(topmost)
    }

    pub fn set_topmost(&mut self, window: &WindowInfo, enabled: bool) -> Result<(), String> {
        let tracked = self.tracked_window(window)?;
        let key = window.hwnd as usize;
        if self.topmost_actions.contains_key(&key) {
            return Err(window_topmost::pending_message());
        }
        let original = self.is_topmost(window)?;
        // 写入前保存恢复快照，失败或部分生效时也能通过恢复操作撤销
        if let Some(current) = self.changed.get_mut(&(window.hwnd as usize)) {
            current.original_topmost.get_or_insert(original);
        }
        if !self.write_topmost(&tracked, enabled)? {
            self.defer_topmost(key, TopmostAction::Set(enabled));
            return Ok(());
        }
        if let Some(current) = self.changed.get_mut(&(window.hwnd as usize)) {
            current.desired_topmost = Some(enabled);
        }
        Ok(())
    }

    fn write_topmost(&self, tracked: &ChangedWindow, enabled: bool) -> Result<bool, String> {
        self.require_owned(tracked)?;
        let key = tracked.window.hwnd as usize;
        self.require_container_ready(key)?;
        let complete = if let Some(container) = self.containers.get(&key) {
            container.set_topmost(enabled)?
        } else {
            set_window_topmost(
                tracked.window.hwnd,
                enabled,
                &tracked.window,
                &self.property_name,
                tracked.token,
            )?
        };
        self.require_owned(tracked)?;
        Ok(complete)
    }

    pub fn finish_topmost_requests(&mut self) -> Vec<String> {
        let completed = window_topmost::take_completed();
        if completed.is_empty() {
            return Vec::new();
        }
        let mut errors: Vec<_> = completed
            .iter()
            .filter_map(|(_, result)| result.as_ref().err().cloned())
            .collect();
        let actions = std::mem::take(&mut self.topmost_actions);
        for (key, pending_action) in actions {
            let Some(tracked) = self
                .changed
                .get(&key)
                .filter(|tracked| tracked.token == pending_action.token)
                .cloned()
            else {
                continue;
            };
            let pending = window_topmost::is_pending(tracked.window.hwnd)
                || self
                    .containers
                    .get(&key)
                    .is_some_and(WindowContainer::has_pending_topmost);
            if pending {
                self.topmost_actions.insert(key, pending_action);
                continue;
            }
            let failed = completed.iter().any(|(hwnd, result)| {
                result.is_err()
                    && (*hwnd == tracked.window.hwnd
                        || self
                            .containers
                            .get(&key)
                            .is_some_and(|container| container.hwnd().ok() == Some(*hwnd)))
            });
            let action = pending_action.action;
            if failed && !matches!(action, TopmostAction::Restore) {
                continue;
            }
            let result = match action {
                TopmostAction::Set(enabled) => self.set_topmost(&tracked.window, enabled),
                TopmostAction::DisableContainer => self.set_container_mode(&tracked.window, false),
                TopmostAction::Restore => self.restore_tracked(&tracked),
            };
            if let Err(error) = result {
                if !self.topmost_actions.contains_key(&key) {
                    errors.push(error);
                }
            }
        }
        errors
    }

    fn defer_topmost(&mut self, key: usize, action: TopmostAction) {
        if let Some(tracked) = self.changed.get(&key) {
            self.topmost_actions.insert(
                key,
                PendingTopmostAction {
                    token: tracked.token,
                    action,
                },
            );
        }
    }

    pub fn set_taskbar_hidden(&mut self, window: &WindowInfo, hidden: bool) -> Result<(), String> {
        let tracked = self.tracked_window(window)?;
        self.require_container_ready(window.hwnd as usize)?;
        if !hidden && !tracked.taskbar_hidden {
            return Ok(());
        }
        if hidden && tracked.original_taskbar_visible.is_none() {
            if !self.containers.contains_key(&(window.hwnd as usize)) {
                validate_taskbar_candidate(window)?;
            }
        }
        let taskbar = TaskbarList::new()?;
        self.require_owned(&tracked)?;
        taskbar.set_hidden(self.taskbar_window(&tracked)?, hidden)?;
        self.require_owned(&tracked)?;
        if let Some(current) = self.changed.get_mut(&(window.hwnd as usize)) {
            if current.original_taskbar_visible.is_none() {
                // 标准任务栏资格只能推定原按钮存在，ITaskbarList 没有查询接口
                current.original_taskbar_visible = Some(true);
            }
            current.taskbar_hidden = hidden;
            current.taskbar_maintenance_enabled = hidden;
        }
        Ok(())
    }

    pub fn is_taskbar_hidden(&self, window: &WindowInfo) -> bool {
        self.changed
            .get(&(window.hwnd as usize))
            .is_some_and(|tracked| {
                tracked.taskbar_hidden
                    && same_identity(&tracked.window, window)
                    && self.owns(tracked)
            })
    }

    pub fn reapply_taskbar_hidden(&mut self) -> Vec<String> {
        let pending: Vec<_> = self
            .changed
            .values()
            .filter(|tracked| tracked.taskbar_hidden && tracked.taskbar_maintenance_enabled)
            .cloned()
            .collect();
        self.maintain_taskbar_targets(pending, false)
    }

    pub fn maintain_taskbar_hidden(&mut self, hwnd: HWND, token: usize) -> Vec<String> {
        let Some(tracked) = self
            .changed
            .get(&(hwnd as usize))
            .filter(|tracked| {
                tracked.token == token
                    && tracked.taskbar_hidden
                    && tracked.taskbar_maintenance_enabled
            })
            .cloned()
        else {
            return Vec::new();
        };
        self.maintain_taskbar_targets(vec![tracked], true)
    }

    fn maintain_taskbar_targets(
        &mut self,
        mut pending: Vec<ChangedWindow>,
        visible_only: bool,
    ) -> Vec<String> {
        pending.sort_by_key(|tracked| tracked.token);
        let mut failures = Vec::new();
        pending.retain(|tracked| {
            if self
                .container_restores
                .contains(&(tracked.window.hwnd as usize))
            {
                return false;
            }
            if !self.owns(tracked) {
                self.forget_stale(tracked.window.hwnd as usize);
                return false;
            }
            match self.taskbar_window(tracked) {
                Ok(hwnd) => !visible_only || is_visible_restored(hwnd),
                Err(error) => {
                    failures.push(format!(
                        "PID {} / HWND 0x{:X}：{error}",
                        tracked.window.process_id, tracked.window.hwnd as usize
                    ));
                    false
                }
            }
        });
        if pending.is_empty() {
            return failures;
        }
        let taskbar = match TaskbarList::new() {
            Ok(taskbar) => taskbar,
            Err(error) => {
                failures.push(error);
                return failures;
            }
        };
        for tracked in pending {
            if !self.owns(&tracked) {
                self.forget_stale(tracked.window.hwnd as usize);
                continue;
            }
            // COM 初始化可能重入消息处理，调用前重新核对宿主实例
            let taskbar_window = match self.taskbar_window(&tracked) {
                Ok(hwnd) => hwnd,
                Err(error) => {
                    failures.push(format!(
                        "PID {} / HWND 0x{:X}：{error}",
                        tracked.window.process_id, tracked.window.hwnd as usize
                    ));
                    continue;
                }
            };
            if visible_only && !is_visible_restored(taskbar_window) {
                continue;
            }
            if let Err(error) = taskbar.set_hidden(taskbar_window, true) {
                failures.push(format!(
                    "PID {} / HWND 0x{:X}：{error}",
                    tracked.window.process_id, tracked.window.hwnd as usize
                ));
            }
            if !self.owns(&tracked) {
                self.forget_stale(tracked.window.hwnd as usize);
            }
        }
        failures
    }

    pub fn restore(&mut self, window: &WindowInfo) -> Result<(), String> {
        let key = window.hwnd as usize;
        let Some(tracked) = self.changed.get(&key).cloned() else {
            return Ok(());
        };
        if !self.owns(&tracked) && !self.containers.contains_key(&key) {
            self.forget_stale(key);
            return Err(tr("原窗口已关闭或窗口标记已失效，已清理旧记录", "The original window has closed or its marker is invalid; the stale record was removed").into());
        }
        if !same_identity(&tracked.window, window) {
            return Err(tr(
                "选中的窗口身份与已修改窗口不一致，请刷新列表",
                "The selected window does not match the modified window; refresh the list",
            )
            .into());
        }
        self.restore_tracked(&tracked)
    }

    pub fn restore_all(&mut self) -> Vec<String> {
        let pending: Vec<_> = self.changed.values().cloned().collect();
        let mut failures = Vec::new();
        for tracked in pending {
            if !self.owns(&tracked)
                && !self
                    .containers
                    .contains_key(&(tracked.window.hwnd as usize))
            {
                // 关闭窗口的属性会随窗口销毁，不能对复用该句柄的新窗口写入
                self.forget_stale(tracked.window.hwnd as usize);
                continue;
            }
            if let Err(error) = self.restore_tracked(&tracked) {
                failures.push(format!(
                    "PID {} / HWND 0x{:X}：{error}",
                    tracked.window.process_id, tracked.window.hwnd as usize
                ));
            }
        }
        // 管理记录失效也不能忽略仍在执行的原生请求，正常退出须等待收尾
        if failures.is_empty() && window_topmost::has_pending_requests() {
            failures.push(window_topmost::pending_message());
        }
        failures
    }

    pub fn changed_windows(&mut self) -> Vec<ChangedWindowInfo> {
        let mut pending: Vec<_> = self.changed.values().cloned().collect();
        // 令牌只在首次登记时分配，调整或切换窗口不改变列表顺序
        pending.sort_by_key(|tracked| tracked.token);
        let mut windows = Vec::with_capacity(pending.len());
        for tracked in pending {
            let key = tracked.window.hwnd as usize;
            if !self.owns(&tracked) && !self.containers.contains_key(&key) {
                self.forget_stale(key);
                continue;
            }
            let mut window = tracked.window.clone();
            if let Some(title) = current_title(&window) {
                window.title = title;
            }
            let observed = if let Some(container) = self.containers.get(&key) {
                Ok(container.transparency())
            } else {
                validate_target(&window)
                    .and_then(|_| snapshot(window.hwnd))
                    .map(snapshot_transparency)
            };
            let transparency = observed
                .as_ref()
                .copied()
                .unwrap_or(tracked.last_transparency);
            let topmost = self.observed_topmost(&tracked).unwrap_or(false);
            if !self.owns(&tracked) && !self.containers.contains_key(&key) {
                self.forget_stale(key);
                continue;
            }
            if let Some(current) = self.changed.get_mut(&key) {
                current.window.title = window.title.clone();
                if observed.is_ok() {
                    current.last_transparency = transparency;
                }
            }
            windows.push(ChangedWindowInfo {
                window,
                token: tracked.token,
                transparency,
                taskbar_hidden: tracked.taskbar_hidden,
                container_mode: self.containers.contains_key(&key),
                topmost,
            });
        }
        windows
    }

    pub fn changed_count(&self) -> usize {
        self.changed
            .values()
            .filter(|tracked| {
                self.owns(tracked)
                    || self
                        .containers
                        .contains_key(&(tracked.window.hwnd as usize))
            })
            .count()
    }

    pub fn is_changed(&self, window: &WindowInfo) -> bool {
        self.changed
            .get(&(window.hwnd as usize))
            .is_some_and(|tracked| {
                same_identity(&tracked.window, window)
                    && (self.owns(tracked) || self.containers.contains_key(&(window.hwnd as usize)))
            })
    }

    fn restore_tracked(&mut self, tracked: &ChangedWindow) -> Result<(), String> {
        let key = tracked.window.hwnd as usize;
        if let Some(current) = self.changed.get_mut(&(tracked.window.hwnd as usize)) {
            if current.token == tracked.token && same_identity(&current.window, &tracked.window) {
                // 恢复请求立即撤销自动维护，失败时也不能重新施加透明度或隐藏任务栏
                current.desired_transparency = None;
                current.desired_topmost = None;
                current.taskbar_maintenance_enabled = false;
                current.clear_maintenance_failures();
            }
        }
        // 必须先将外部窗口移出宿主，失败时保留容器和恢复记录
        if window_topmost::is_pending(tracked.window.hwnd)
            || self
                .containers
                .get(&key)
                .is_some_and(WindowContainer::has_pending_topmost)
            || !self.detach_container(key)?
        {
            self.defer_topmost(key, TopmostAction::Restore);
            return Err(window_topmost::pending_message());
        }
        if window_identity_gone(&tracked.window) {
            self.changed.remove(&(tracked.window.hwnd as usize));
            return Ok(());
        }
        let restored: Result<(), String> = (|| {
            let transparency_error = if tracked.transparency_modified {
                self.write_snapshot(tracked, tracked.original).err()
            } else {
                None
            };
            let taskbar_error = self.restore_taskbar(tracked).err();
            let topmost_error = if let Some(original) = tracked.original_topmost {
                self.require_owned(tracked)?;
                let restored = restore_window_topmost(
                    tracked.window.hwnd,
                    original,
                    &tracked.window,
                    &self.property_name,
                    tracked.token,
                );
                self.require_owned(tracked)?;
                match restored {
                    Ok(true) => None,
                    Ok(false) => {
                        self.defer_topmost(key, TopmostAction::Restore);
                        Some(window_topmost::pending_message())
                    }
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            let errors: Vec<_> = [transparency_error, taskbar_error, topmost_error]
                .into_iter()
                .flatten()
                .collect();
            if !errors.is_empty() {
                return Err(errors.join(tr("；", "; ")));
            }
            self.remove_marker(tracked)
        })();
        // 还原期间原目标可能自行销毁，只清理旧记录，不接管新建窗口
        if restored.is_ok() || window_identity_gone(&tracked.window) {
            self.changed.remove(&(tracked.window.hwnd as usize));
            Ok(())
        } else {
            restored
        }
    }

    fn restore_taskbar(&mut self, tracked: &ChangedWindow) -> Result<(), String> {
        if !tracked.taskbar_hidden {
            return Ok(());
        }
        let original_visible = tracked.original_taskbar_visible.ok_or(tr(
            "缺少原始任务栏状态，已保留恢复记录",
            "The original taskbar state is missing; the recovery record was retained",
        ))?;
        let taskbar = TaskbarList::new()?;
        self.require_owned(tracked)?;
        taskbar.set_hidden(tracked.window.hwnd, !original_visible)?;
        self.require_owned(tracked)?;
        if let Some(current) = self.changed.get_mut(&(tracked.window.hwnd as usize)) {
            current.taskbar_hidden = false;
            current.taskbar_maintenance_enabled = false;
        }
        Ok(())
    }

    fn tracked_window(&self, window: &WindowInfo) -> Result<ChangedWindow, String> {
        let tracked = self.changed.get(&(window.hwnd as usize)).ok_or(tr(
            "该窗口不在已调整列表中，请刷新列表后重试",
            "The window is not in the managed list; refresh the list and try again",
        ))?;
        if !same_identity(&tracked.window, window) {
            return Err(tr(
                "目标窗口身份已变化，请刷新列表后重试",
                "The target window identity changed; refresh the list and try again",
            )
            .into());
        }
        self.require_owned(tracked)?;
        Ok(tracked.clone())
    }

    fn owns(&self, tracked: &ChangedWindow) -> bool {
        identity_matches(&tracked.window)
            && unsafe {
                GetPropW(tracked.window.hwnd, self.property_name.as_ptr()) as usize == tracked.token
            }
    }

    fn owns_selection(&self, selection: &SelectedWindow) -> bool {
        identity_matches(&selection.window)
            && unsafe {
                GetPropW(selection.window.hwnd, self.selection_property_name.as_ptr()) as usize
                    == selection.token
            }
    }

    fn require_selection(&self, window: &WindowInfo) -> Result<(), String> {
        if self.selection.as_ref().is_some_and(|selection| {
            same_identity(&selection.window, window) && self.owns_selection(selection)
        }) {
            Ok(())
        } else {
            Err(tr("选中的窗口已关闭、发生变化或选择标记已失效，请重新选择窗口后调整", "The selected window has closed, changed, or lost its selection marker; select it again before adjusting").into())
        }
    }

    fn require_owned(&self, tracked: &ChangedWindow) -> Result<(), String> {
        if self.owns(tracked) {
            Ok(())
        } else {
            Err(tr(
                "目标窗口已关闭或窗口标记已失效，已停止修改",
                "The target window has closed or its marker is invalid; changes were stopped",
            )
            .into())
        }
    }

    fn note_transparency_mutation(&mut self, tracked: &ChangedWindow) {
        if let Some(current) = self.changed.get_mut(&(tracked.window.hwnd as usize)) {
            if current.token == tracked.token && same_identity(&current.window, &tracked.window) {
                current.transparency_modified = true;
            }
        }
    }

    fn record_maintenance_failure(
        &mut self,
        tracked: &ChangedWindow,
        error: String,
        failures: &mut Vec<String>,
    ) {
        if let Some(current) = self.changed.get_mut(&(tracked.window.hwnd as usize)) {
            if current.token == tracked.token && same_identity(&current.window, &tracked.window) {
                // 消息去重不影响退避，重复失败仍按次数延长暂停时间
                current.maintenance_failures = current.maintenance_failures.saturating_add(1);
                let delay = match current.maintenance_failures {
                    1 => 2,
                    2 => 4,
                    3 => 8,
                    4 => 16,
                    _ => 30,
                };
                current.next_maintenance_retry = Some(Instant::now() + Duration::from_secs(delay));
                if current.maintenance_errors.insert(error.clone()) {
                    failures.push(format!(
                        "PID {} / HWND 0x{:X}：{error}",
                        tracked.window.process_id, tracked.window.hwnd as usize
                    ));
                }
            }
        }
    }

    fn write_snapshot(&mut self, tracked: &ChangedWindow, desired: Snapshot) -> Result<(), String> {
        self.require_owned(tracked)?;
        let hwnd = tracked.window.hwnd;
        let current_style = extended_style(hwnd)?;
        let layered_bit = WS_EX_LAYERED as isize;
        let desired_style = if desired.layered {
            current_style | layered_bit
        } else {
            current_style & !layered_bit
        };
        let style_changed = current_style != desired_style;
        if style_changed {
            self.require_owned(tracked)?;
            set_extended_style(hwnd, desired_style)?;
            self.note_transparency_mutation(tracked);
        }
        if let Some(attributes) = desired.attributes {
            self.require_owned(tracked)?;
            unsafe {
                SetLastError(0);
                if SetLayeredWindowAttributes(
                    hwnd,
                    attributes.color_key,
                    attributes.alpha,
                    attributes.flags,
                ) == 0
                {
                    return Err(api_error(
                        tr("无法设置窗口透明度", "Cannot set window transparency"),
                        GetLastError(),
                    ));
                }
            }
            self.note_transparency_mutation(tracked);
        }
        if style_changed && !desired.layered {
            self.require_owned(tracked)?;
            unsafe {
                SetLastError(0);
                if RedrawWindow(
                    hwnd,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    RDW_ERASE | RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
                ) == 0
                {
                    return Err(api_error(
                        tr("无法请求窗口重绘", "Cannot request a window redraw"),
                        GetLastError(),
                    ));
                }
            }
        }
        self.require_owned(tracked)
    }

    fn remove_marker(&self, tracked: &ChangedWindow) -> Result<(), String> {
        self.require_owned(tracked)?;
        unsafe {
            SetLastError(0);
            let removed = RemovePropW(tracked.window.hwnd, self.property_name.as_ptr());
            if removed as usize != tracked.token {
                return Err(api_error(
                    tr("无法清理窗口标记", "Cannot remove the window marker"),
                    GetLastError(),
                ));
            }
        }
        Ok(())
    }

    fn remove_marker_if_owned(&self, tracked: &ChangedWindow) {
        if self.owns(tracked) {
            unsafe {
                RemovePropW(tracked.window.hwnd, self.property_name.as_ptr());
            }
        }
    }
}

impl Default for WindowController {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WindowController {
    fn drop(&mut self) {
        self.clear_selection();
        let _ = self.restore_all();
    }
}

pub fn enumerate_windows() -> Vec<WindowInfo> {
    let mut windows = Vec::<WindowInfo>::new();
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut windows as *mut Vec<WindowInfo> as LPARAM,
        );
    }
    windows.sort_by_cached_key(|window| {
        (
            window.title.to_lowercase(),
            window.process_id,
            window.hwnd as usize,
        )
    });
    windows
}

unsafe extern "system" fn collect_window(hwnd: HWND, parameter: LPARAM) -> i32 {
    if let Some(window) = describe_window(hwnd) {
        let windows = &mut *(parameter as *mut Vec<WindowInfo>);
        windows.push(window);
    }
    1
}

pub fn describe_window(hwnd: HWND) -> Option<WindowInfo> {
    unsafe {
        if hwnd.is_null() || IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 {
            return None;
        }
        if GetAncestor(hwnd, GA_ROOT) != hwnd || protected_window(hwnd) {
            return None;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(hwnd, &mut process_id);
        if thread_id == 0 || process_id == 0 || process_id == GetCurrentProcessId() {
            return None;
        }
        let style = extended_style(hwnd).ok()?;
        if style & WS_EX_TOOLWINDOW as isize != 0 || cloaked_or_unknown(hwnd) {
            return None;
        }

        // 外进程顶层窗口只读取系统保存的标题，不发送 WM_GETTEXT
        let mut title = [0u16; 1024];
        let length = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if length <= 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&title[..length as usize]);
        if title.trim().is_empty() {
            return None;
        }
        let window = WindowInfo {
            hwnd,
            title,
            process_id,
            thread_id,
        };
        if identity_matches(&window) {
            Some(window)
        } else {
            None
        }
    }
}

pub fn transparency_of(window: &WindowInfo) -> Result<u8, String> {
    validate_target(window)?;
    let current = snapshot(window.hwnd)?;
    if !identity_matches(window) {
        return Err(tr(
            "目标窗口已关闭或发生变化，请刷新列表",
            "The target window has closed or changed; refresh the list",
        )
        .into());
    }
    Ok(snapshot_transparency(current))
}

fn snapshot_transparency(current: Snapshot) -> u8 {
    let alpha = snapshot_alpha(current);
    (((255 - alpha as u32) * 100 + 127) / 255) as u8
}

fn snapshot_alpha(current: Snapshot) -> u8 {
    current
        .attributes
        .filter(|attributes| attributes.flags & LWA_ALPHA != 0)
        .map_or(255, |attributes| attributes.alpha)
}

fn transparency_alpha(transparency: u8) -> u8 {
    (((100 - transparency as u32) * 255 + 50) / 100) as u8
}

fn snapshot_with_transparency(current: Snapshot, transparency: u8) -> Snapshot {
    let attributes = current.attributes.unwrap_or(LayeredAttributes {
        color_key: 0,
        alpha: 255,
        flags: 0,
    });
    Snapshot {
        layered: true,
        attributes: Some(LayeredAttributes {
            alpha: transparency_alpha(transparency),
            flags: attributes.flags | LWA_ALPHA,
            ..attributes
        }),
    }
}

fn is_visible_restored(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 }
}

fn current_title(window: &WindowInfo) -> Option<String> {
    if !identity_matches(window) || window.process_id == unsafe { GetCurrentProcessId() } {
        return None;
    }
    let mut title = [0u16; 1024];
    unsafe {
        SetLastError(0);
        let length = GetWindowTextW(window.hwnd, title.as_mut_ptr(), title.len() as i32);
        if length == 0 && GetLastError() != 0 {
            return None;
        }
        if !identity_matches(window) {
            return None;
        }
        Some(String::from_utf16_lossy(&title[..length as usize]))
    }
}

pub(crate) fn window_is_topmost(hwnd: HWND) -> Result<bool, String> {
    window_topmost::is_topmost(hwnd)
}

pub(crate) fn set_window_topmost(
    hwnd: HWND,
    enabled: bool,
    window: &WindowInfo,
    property: &[u16],
    token: usize,
) -> Result<bool, String> {
    window_topmost::request(
        hwnd,
        enabled,
        None,
        window_topmost::Authority::new(window, property, token),
    )
}

pub(crate) fn restore_window_topmost(
    hwnd: HWND,
    enabled: bool,
    window: &WindowInfo,
    property: &[u16],
    token: usize,
) -> Result<bool, String> {
    window_topmost::request(
        hwnd,
        enabled,
        None,
        window_topmost::Authority::for_restore(window, property, token),
    )
}

fn snapshot(hwnd: HWND) -> Result<Snapshot, String> {
    let style = extended_style(hwnd)?;
    let layered = style & WS_EX_LAYERED as isize != 0;
    let attributes = if layered {
        let mut attributes = LayeredAttributes {
            color_key: 0,
            alpha: 0,
            flags: 0,
        };
        unsafe {
            SetLastError(0);
            if GetLayeredWindowAttributes(
                hwnd,
                &mut attributes.color_key,
                &mut attributes.alpha,
                &mut attributes.flags,
            ) == 0
            {
                let error = GetLastError();
                if error == ERROR_ACCESS_DENIED || error == ERROR_PRIVILEGE_NOT_HELD {
                    return Err(api_error(
                        tr(
                            "无法读取窗口原始分层属性",
                            "Cannot read the original layered window attributes",
                        ),
                        error,
                    ));
                }
                return Err(format!(
                    "{}{}{}",
                    tr("无法读取窗口原始分层属性", "Cannot read the original layered window attributes"),
                    if error == 0 { String::new() } else { format!("{} {error}{}", tr("（错误", " (error"), tr("）", ")")) },
                    tr("；该窗口可能使用逐像素透明，无法安全修改，已保留原状", "; the window may use per-pixel transparency and cannot be safely modified; its original state was retained")
                ));
            }
        }
        Some(attributes)
    } else {
        None
    };
    Ok(Snapshot {
        layered,
        attributes,
    })
}

fn extended_style(hwnd: HWND) -> Result<isize, String> {
    unsafe {
        SetLastError(0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as isize;
        let error = GetLastError();
        if style == 0 && error != 0 {
            Err(api_error(
                tr("无法读取窗口样式", "Cannot read the window style"),
                error,
            ))
        } else {
            Ok(style)
        }
    }
}

fn set_extended_style(hwnd: HWND, style: isize) -> Result<(), String> {
    unsafe {
        SetLastError(0);
        let previous = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as _);
        let error = GetLastError();
        if previous == 0 && error != 0 {
            Err(api_error(
                tr(
                    "无法设置窗口分层样式",
                    "Cannot set the layered window style",
                ),
                error,
            ))
        } else {
            Ok(())
        }
    }
}

fn validate_taskbar_candidate(window: &WindowInfo) -> Result<(), String> {
    validate_target(window)?;
    let style = extended_style(window.hwnd)?;
    unsafe {
        if IsWindowVisible(window.hwnd) == 0 || cloaked_or_unknown(window.hwnd) {
            return Err(tr("当前窗口未显示或可见性未知，不能安全隐藏任务栏按钮", "The window is not visible or its visibility is unknown; its taskbar button cannot be safely hidden").into());
        }
        if style & WS_EX_TOOLWINDOW as isize != 0 {
            return Err(tr(
                "工具窗口没有标准任务栏按钮，不能安全隐藏",
                "Tool windows have no standard taskbar button and cannot be safely hidden",
            )
            .into());
        }
        if style & WS_EX_NOACTIVATE as isize != 0 && style & WS_EX_APPWINDOW as isize == 0 {
            return Err(tr("该窗口不具备标准任务栏资格，不能安全隐藏", "The window is not eligible for a standard taskbar button and cannot be safely hidden").into());
        }
        SetLastError(0);
        let owner = GetWindow(window.hwnd, GW_OWNER);
        let error = GetLastError();
        if owner.is_null() && error != 0 {
            return Err(api_error(
                tr(
                    "无法核对窗口任务栏资格",
                    "Cannot verify taskbar eligibility for the window",
                ),
                error,
            ));
        }
        if !owner.is_null() {
            return Err(tr("该窗口具有所属窗口，无法确定原任务栏状态，不能安全隐藏", "The window has an owner; its original taskbar state is unknown and cannot be safely hidden").into());
        }
    }
    if !identity_matches(window) {
        return Err(tr(
            "目标窗口已关闭或发生变化，请刷新列表后重试",
            "The target window has closed or changed; refresh the list and try again",
        )
        .into());
    }
    Ok(())
}

fn validate_target(window: &WindowInfo) -> Result<(), String> {
    if !identity_matches(window) {
        return Err(tr(
            "目标窗口已关闭或发生变化，请刷新列表后重新选择",
            "The target window has closed or changed; refresh the list and select again",
        )
        .into());
    }
    unsafe {
        if window.process_id == GetCurrentProcessId() {
            return Err(tr(
                "不能修改本软件自身的窗口",
                "The application's own window cannot be modified",
            )
            .into());
        }
        if GetAncestor(window.hwnd, GA_ROOT) != window.hwnd || protected_window(window.hwnd) {
            return Err(tr(
                "不能修改桌面、任务栏或系统窗口",
                "Desktop, taskbar, and system windows cannot be modified",
            )
            .into());
        }
    }
    Ok(())
}

fn identity_matches(window: &WindowInfo) -> bool {
    unsafe {
        if window.hwnd.is_null() || IsWindow(window.hwnd) == 0 {
            return false;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(window.hwnd, &mut process_id);
        thread_id != 0 && thread_id == window.thread_id && process_id == window.process_id
    }
}

fn window_identity_gone(window: &WindowInfo) -> bool {
    unsafe {
        if window.hwnd.is_null() || IsWindow(window.hwnd) == 0 {
            return true;
        }
        let mut process_id = 0;
        let thread_id = GetWindowThreadProcessId(window.hwnd, &mut process_id);
        // 身份读取失败不能推断窗口已销毁，令牌丢失也不属于销毁
        thread_id != 0
            && process_id != 0
            && (thread_id != window.thread_id || process_id != window.process_id)
    }
}

fn same_identity(left: &WindowInfo, right: &WindowInfo) -> bool {
    left.hwnd == right.hwnd
        && left.process_id == right.process_id
        && left.thread_id == right.thread_id
}

unsafe fn protected_window(hwnd: HWND) -> bool {
    if hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
        return true;
    }
    let mut class = [0u16; 256];
    let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    if length <= 0 {
        return true;
    }
    matches!(
        String::from_utf16_lossy(&class[..length as usize]).as_str(),
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "#32769"
            | "MultitaskingViewFrame"
    )
}

unsafe fn cloaked_or_unknown(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    let result = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        &mut cloaked as *mut u32 as *mut c_void,
        size_of::<u32>() as u32,
    );
    result < 0 || cloaked != 0
}

fn next_id() -> usize {
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

fn api_error(action: &str, code: u32) -> String {
    let detail = if code == 0 {
        tr(
            "系统未返回具体错误码",
            "The system did not provide an error code",
        )
        .to_string()
    } else {
        format!(
            "{}{} {code}",
            std::io::Error::from_raw_os_error(code as i32),
            tr("，错误", "; error")
        )
    };
    if code == ERROR_ACCESS_DENIED || code == ERROR_PRIVILEGE_NOT_HELD {
        format!(
            "{action}{}{detail}{}",
            tr("：", ": "),
            tr(
                "。目标可能以更高权限运行，可尝试以管理员身份启动本软件",
                ". The target may run with elevated privileges; try running Trans as administrator"
            )
        )
    } else {
        format!("{action}{}{detail}", tr("：", ": "))
    }
}
