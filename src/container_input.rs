use std::cell::RefCell;
use std::marker::PhantomData;
use std::ptr::null_mut;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::PhysicalToLogicalPointForPerMonitorDPI;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::localization::tr;
use crate::window_control::WindowInfo;

pub(crate) const INPUT_ACTION: u32 = WM_APP + 0x420;
pub(crate) const INPUT_DOUBLE_CLICK: isize = 1 << 16;
const ACTION_TICKET_SHIFT: u32 = 17;
const MAX_ACTION_TICKET: usize = (isize::MAX as usize) >> ACTION_TICKET_SHIFT;

static NEXT_REGISTRATION: AtomicUsize = AtomicUsize::new(1);
static HOST_PROPERTY: OnceLock<Vec<u16>> = OnceLock::new();

thread_local! {
    static INPUT: RefCell<InputRegistry> = const { RefCell::new(InputRegistry {
        hook: null_mut(),
        registrations: Vec::new(),
        presses: PressState::new(),
        last_click: None,
    }) };
}

pub(crate) struct ContainerInput {
    registration: Rc<Registration>,
    _thread_bound: PhantomData<Rc<()>>,
}

struct Registration {
    nonce: usize,
    host: HWND,
    host_nonce: usize,
    host_thread: u32,
    target: WindowInfo,
    property: Vec<u16>,
    token: usize,
}

struct InputRegistry {
    hook: HHOOK,
    registrations: Vec<Rc<Registration>>,
    presses: PressState,
    last_click: Option<CaptionClick>,
}

#[derive(Clone, Copy)]
struct PendingPress {
    registration: usize,
    action: LPARAM,
    hit: u32,
    double_click: bool,
    point: POINT,
    time: u32,
    started: bool,
}

impl PendingPress {
    fn is_button(self) -> bool {
        matches!(self.hit, HTMINBUTTON | HTMAXBUTTON | HTCLOSE)
    }

    fn swallow_up(self) -> bool {
        self.is_button() || self.double_click
    }
}

struct PressState {
    next_ticket: usize,
    pending: Option<PendingPress>,
}

impl PressState {
    const fn new() -> Self {
        Self {
            next_ticket: 0,
            pending: None,
        }
    }

    fn begin(
        &mut self,
        registration: usize,
        hit: u32,
        double_click: bool,
        point: POINT,
        time: u32,
    ) -> Option<PendingPress> {
        self.pending = None;
        if registration == 0 {
            return None;
        }
        self.next_ticket = if self.next_ticket >= MAX_ACTION_TICKET {
            1
        } else {
            self.next_ticket + 1
        };
        let action = encode_action(self.next_ticket, hit, double_click)?;
        let press = PendingPress {
            registration,
            action,
            hit,
            double_click,
            point,
            time,
            started: false,
        };
        self.pending = Some(press);
        Some(press)
    }

    fn claim_drag(&mut self, registration: usize, action: LPARAM) -> Option<POINT> {
        let press = self.pending.as_mut()?;
        if press.registration != registration
            || press.action != action
            || press.started
            || press.double_click
            || press.is_button()
        {
            return None;
        }
        press.started = true;
        Some(press.point)
    }

    fn release(&mut self) -> Option<PendingPress> {
        self.pending.take()
    }

    fn cancel(&mut self) {
        self.pending = None;
    }

    fn cancel_registration(&mut self, registration: usize) {
        if self
            .pending
            .is_some_and(|press| press.registration == registration)
        {
            self.pending = None;
        }
    }

    fn post_failed(&mut self, registration: usize, action: LPARAM) {
        if self
            .pending
            .is_some_and(|press| press.registration == registration && press.action == action)
        {
            self.pending = None;
        }
    }
}

#[derive(Clone, Copy)]
struct CaptionClick {
    nonce: usize,
    point: POINT,
    time: u32,
}

impl ContainerInput {
    pub(crate) fn new(
        host: HWND,
        target: &WindowInfo,
        ownership_property: &[u16],
        token: usize,
    ) -> Result<Self, String> {
        if ownership_property.last() != Some(&0) || token == 0 {
            return Err(tr(
                "无法建立容器输入连接：窗口标记无效",
                "Cannot connect container input: invalid window marker",
            )
            .into());
        }
        let mut nonce = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        if nonce == 0 {
            nonce = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        }
        let registration = Rc::new(Registration {
            nonce,
            host,
            host_nonce: unsafe { GetPropW(host, host_property().as_ptr()) } as usize,
            host_thread: unsafe { GetCurrentThreadId() },
            target: target.clone(),
            property: ownership_property.to_vec(),
            token,
        });
        if !unsafe { registration.valid(false) } {
            return Err(tr(
                "无法建立容器输入连接：窗口已关闭或发生变化",
                "Cannot connect container input: a window has closed or changed",
            )
            .into());
        }

        let needs_hook = INPUT.with(|input| input.borrow().hook.is_null());
        if needs_hook {
            // 低级钩子在当前 UI 线程回调，仅在至少一个独立容器存在时启用
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(mouse_hook),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                )
            };
            if hook.is_null() {
                let code = unsafe { GetLastError() };
                return Err(format!(
                    "{}: {}",
                    tr("无法建立容器输入连接", "Cannot connect container input"),
                    std::io::Error::from_raw_os_error(code as i32)
                ));
            }
            INPUT.with(|input| input.borrow_mut().hook = hook);
        }
        INPUT.with(|input| {
            input.borrow_mut().registrations.push(registration.clone());
        });
        Ok(Self {
            registration,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn accepts(&self, token: usize) -> bool {
        token == self.registration.nonce
            && unsafe { self.registration.valid(true) }
            && registered(self.registration.nonce)
    }

    pub(crate) fn claim_drag(&self, action: LPARAM) -> Option<POINT> {
        if !unsafe { self.registration.valid(true) } || !registered(self.registration.nonce) {
            return None;
        }
        INPUT.with(|input| {
            input
                .borrow_mut()
                .presses
                .claim_drag(self.registration.nonce, action)
        })
    }
}

impl Drop for ContainerInput {
    fn drop(&mut self) {
        let nonce = self.registration.nonce;
        let hook = INPUT.with(|input| {
            let mut input = input.borrow_mut();
            input.registrations.retain(|entry| entry.nonce != nonce);
            input.presses.cancel_registration(nonce);
            if input.last_click.is_some_and(|click| click.nonce == nonce) {
                input.last_click = None;
            }
            if input.registrations.is_empty() {
                std::mem::replace(&mut input.hook, null_mut())
            } else {
                null_mut()
            }
        });
        if !hook.is_null() {
            unsafe { UnhookWindowsHookEx(hook) };
        }
    }
}

impl Drop for InputRegistry {
    fn drop(&mut self) {
        if !self.hook.is_null() {
            unsafe { UnhookWindowsHookEx(self.hook) };
        }
    }
}

impl Registration {
    unsafe fn valid(&self, require_attached: bool) -> bool {
        if self.host.is_null()
            || self.target.hwnd.is_null()
            || self.host_nonce == 0
            || self.host == self.target.hwnd
            || IsWindow(self.host) == 0
            || IsWindow(self.target.hwnd) == 0
            || GetCurrentThreadId() != self.host_thread
        {
            return false;
        }
        let mut host_pid = 0;
        let host_thread = GetWindowThreadProcessId(self.host, &mut host_pid);
        let mut target_pid = 0;
        let target_thread = GetWindowThreadProcessId(self.target.hwnd, &mut target_pid);
        host_pid == GetCurrentProcessId()
            && host_thread == self.host_thread
            && target_pid == self.target.process_id
            && target_thread == self.target.thread_id
            && GetPropW(self.host, host_property().as_ptr()) as usize == self.host_nonce
            && GetPropW(self.target.hwnd, self.property.as_ptr()) as usize == self.token
            && (!require_attached || IsChild(self.host, self.target.hwnd) != 0)
    }

    unsafe fn contains(&self, window: HWND) -> bool {
        !window.is_null() && (window == self.target.hwnd || IsChild(self.target.hwnd, window) != 0)
    }

    unsafe fn hit_test(&self, point: POINT) -> Option<u32> {
        if !self.valid(true) || !self.contains(WindowFromPhysicalPoint(point)) {
            return None;
        }
        let mut target_point = point;
        if PhysicalToLogicalPointForPerMonitorDPI(self.target.hwnd, &mut target_point) == 0 {
            return None;
        }
        let mut hit = 0usize;
        // 只查询已命中的原窗口，超时即放行，不在钩子中执行移动或其他模态操作
        let sent = SendMessageTimeoutW(
            self.target.hwnd,
            WM_NCHITTEST,
            0,
            screen_lparam(target_point),
            SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            10,
            &mut hit,
        );
        if sent == 0
            || !self.valid(true)
            || !self.contains(WindowFromPhysicalPoint(point))
            || hit > u32::MAX as usize
        {
            return None;
        }
        let hit = hit as u32;
        routed_hit(hit).then_some(hit)
    }

    unsafe fn post(&self, action: LPARAM) -> bool {
        self.valid(true)
            && registered(self.nonce)
            && PostMessageW(self.host, INPUT_ACTION, self.nonce, action) != 0
    }
}

pub(crate) fn decode_action(action: LPARAM) -> Option<(u32, bool)> {
    if action <= 0 {
        return None;
    }
    let double_click = action & INPUT_DOUBLE_CLICK != 0;
    let hit = (action as usize & 0xffff) as u32;
    let ticket = (action as usize) >> ACTION_TICKET_SHIFT;
    if ticket == 0 || !routed_hit(hit) || (double_click && hit != HTCAPTION) {
        None
    } else {
        Some((hit, double_click))
    }
}

fn encode_action(ticket: usize, hit: u32, double_click: bool) -> Option<LPARAM> {
    if ticket == 0
        || ticket > MAX_ACTION_TICKET
        || !routed_hit(hit)
        || (double_click && hit != HTCAPTION)
    {
        return None;
    }
    Some(
        ((ticket << ACTION_TICKET_SHIFT) | hit as usize) as LPARAM
            | if double_click { INPUT_DOUBLE_CLICK } else { 0 },
    )
}

fn routed_hit(hit: u32) -> bool {
    hit == HTCAPTION
        || (HTLEFT..=HTBOTTOMRIGHT).contains(&hit)
        || matches!(hit, HTMINBUTTON | HTMAXBUTTON | HTCLOSE)
}

fn registered(nonce: usize) -> bool {
    INPUT.with(|input| {
        input
            .borrow()
            .registrations
            .iter()
            .any(|entry| entry.nonce == nonce)
    })
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32
        || lparam == 0
        || !matches!(wparam as u32, WM_LBUTTONDOWN | WM_LBUTTONUP)
    {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let mouse = &*(lparam as *const MSLLHOOKSTRUCT);
    let swallow = if wparam as u32 == WM_LBUTTONDOWN {
        mouse_down(mouse)
    } else {
        mouse_up(mouse)
    };
    if swallow {
        1
    } else {
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

unsafe fn mouse_down(mouse: &MSLLHOOKSTRUCT) -> bool {
    let (registrations, last_click) = INPUT.with(|input| {
        let mut input = input.borrow_mut();
        input.presses.cancel();
        (input.registrations.clone(), input.last_click.take())
    });
    // 所有跨窗口 API 调用都发生在释放 TLS 借用后
    let under_pointer = WindowFromPhysicalPoint(mouse.pt);
    let Some(registration) = registrations
        .into_iter()
        .find(|entry| entry.contains(under_pointer) && entry.valid(true))
    else {
        return false;
    };
    let Some(hit) = registration.hit_test(mouse.pt) else {
        return false;
    };
    let double_click = hit == HTCAPTION
        && last_click.is_some_and(|click| {
            click.nonce == registration.nonce
                && mouse.time.wrapping_sub(click.time) <= GetDoubleClickTime()
                && nearby(click.point, mouse.pt)
        });
    // 用已捕获的按压生命周期校验排队动作，不依赖被拦截输入的异步键状态
    let press = INPUT.with(|input| {
        input.borrow_mut().presses.begin(
            registration.nonce,
            hit,
            double_click,
            mouse.pt,
            mouse.time,
        )
    });
    let Some(press) = press else {
        return false;
    };
    if !press.is_button() {
        if !registration.post(press.action) {
            INPUT.with(|input| {
                input
                    .borrow_mut()
                    .presses
                    .post_failed(registration.nonce, press.action);
            });
            return false;
        }
    }
    true
}

unsafe fn mouse_up(mouse: &MSLLHOOKSTRUCT) -> bool {
    let press = INPUT.with(|input| input.borrow_mut().presses.release());
    let Some(press) = press else {
        return false;
    };
    let registration = INPUT.with(|input| {
        input
            .borrow()
            .registrations
            .iter()
            .find(|entry| entry.nonce == press.registration)
            .cloned()
    });
    let Some(registration) = registration else {
        return press.swallow_up();
    };
    if press.is_button() {
        // 按钮只在同一原窗口的同一按钮内抬起时触发，移出后松开只取消
        if registration.hit_test(mouse.pt) == Some(press.hit) {
            registration.post(press.action);
        }
    } else if press.hit == HTCAPTION
        && !press.double_click
        && registration.valid(true)
        && registered(registration.nonce)
        && mouse.time.wrapping_sub(press.time) <= GetDoubleClickTime()
        && nearby(press.point, mouse.pt)
    {
        INPUT.with(|input| {
            input.borrow_mut().last_click = Some(CaptionClick {
                nonce: registration.nonce,
                point: press.point,
                time: press.time,
            });
        });
    }
    // 移动和缩放的抬起事件必须交给宿主原生模态循环，用于正常结束拖动
    press.swallow_up()
}

unsafe fn nearby(first: POINT, second: POINT) -> bool {
    (i64::from(first.x) - i64::from(second.x)).abs()
        <= i64::from(GetSystemMetrics(SM_CXDOUBLECLK).max(1)) / 2
        && (i64::from(first.y) - i64::from(second.y)).abs()
            <= i64::from(GetSystemMetrics(SM_CYDOUBLECLK).max(1)) / 2
}

fn screen_lparam(point: POINT) -> LPARAM {
    (u32::from(point.x as u16) | (u32::from(point.y as u16) << 16)) as LPARAM
}

fn host_property() -> &'static [u16] {
    HOST_PROPERTY.get_or_init(|| {
        "Trans.Container.Instance"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(state: &mut PressState, registration: usize, hit: u32) -> PendingPress {
        state
            .begin(registration, hit, false, POINT { x: -240, y: 360 }, 100)
            .expect("valid press")
    }

    fn assert_point(point: Option<POINT>, x: i32, y: i32) {
        let point = point.expect("active press can start dragging");
        assert_eq!((point.x, point.y), (x, y));
    }

    #[test]
    fn caption_and_all_eight_resize_edges_can_start_without_hardware_state() {
        let mut state = PressState::new();
        for hit in std::iter::once(HTCAPTION).chain(HTLEFT..=HTBOTTOMRIGHT) {
            let press = begin(&mut state, 1, hit);
            assert_eq!(decode_action(press.action), Some((hit, false)));
            assert_point(state.claim_drag(1, press.action), -240, 360);
            assert!(!state.release().expect("press still held").swallow_up());
        }
    }

    #[test]
    fn release_before_dispatch_cancels_queued_drag() {
        let mut state = PressState::new();
        let press = begin(&mut state, 1, HTCAPTION);
        state.release();
        assert!(state.claim_drag(1, press.action).is_none());
    }

    #[test]
    fn old_ticket_cannot_borrow_a_new_press_at_the_same_hit() {
        let mut state = PressState::new();
        let old = begin(&mut state, 1, HTCAPTION);
        state.release();
        let current = begin(&mut state, 1, HTCAPTION);
        assert_ne!(old.action, current.action);
        assert!(state.claim_drag(1, old.action).is_none());
        assert_point(state.claim_drag(1, current.action), -240, 360);
    }

    #[test]
    fn each_press_can_be_claimed_only_once() {
        let mut state = PressState::new();
        let press = begin(&mut state, 1, HTRIGHT);
        assert!(state.claim_drag(1, press.action).is_some());
        assert!(state.claim_drag(1, press.action).is_none());
        assert!(state.release().is_some());
        assert!(state.claim_drag(1, press.action).is_none());
    }

    #[test]
    fn another_container_cannot_claim_the_active_press() {
        let mut state = PressState::new();
        let press = begin(&mut state, 1, HTTOPLEFT);
        assert!(state.claim_drag(2, press.action).is_none());
        assert!(state.claim_drag(1, press.action).is_some());
    }

    #[test]
    fn buttons_remain_release_commands_and_cannot_start_a_drag() {
        let mut state = PressState::new();
        for hit in [HTMINBUTTON, HTMAXBUTTON, HTCLOSE] {
            let press = begin(&mut state, 1, hit);
            assert!(state.claim_drag(1, press.action).is_none());
            let released = state.release().expect("button remains held");
            assert!(released.is_button());
            assert!(released.swallow_up());
            assert_eq!(decode_action(released.action), Some((hit, false)));
        }
    }

    #[test]
    fn caption_double_click_remains_a_command_instead_of_dragging() {
        let mut state = PressState::new();
        let press = state
            .begin(1, HTCAPTION, true, POINT { x: 30, y: 40 }, 200)
            .expect("valid double click");
        assert_eq!(decode_action(press.action), Some((HTCAPTION, true)));
        assert!(state.claim_drag(1, press.action).is_none());
        assert!(state.release().expect("double click held").swallow_up());
    }

    #[test]
    fn action_encoding_round_trips_valid_tickets_and_commands() {
        for ticket in [1, 2, MAX_ACTION_TICKET] {
            for hit in std::iter::once(HTCAPTION)
                .chain(HTLEFT..=HTBOTTOMRIGHT)
                .chain([HTMINBUTTON, HTMAXBUTTON, HTCLOSE])
            {
                let action = encode_action(ticket, hit, false).expect("valid action");
                assert!(action > 0);
                assert_eq!((action as usize) >> ACTION_TICKET_SHIFT, ticket);
                assert_eq!(decode_action(action), Some((hit, false)));
            }
            let action = encode_action(ticket, HTCAPTION, true).expect("valid double click");
            assert_eq!(decode_action(action), Some((HTCAPTION, true)));
        }
    }

    #[test]
    fn malformed_actions_and_commands_without_tickets_are_rejected() {
        for action in [
            0,
            -1,
            isize::MIN,
            HTCAPTION as LPARAM,
            HTCAPTION as LPARAM | INPUT_DOUBLE_CLICK,
            (1 << ACTION_TICKET_SHIFT) | HTCLIENT as LPARAM,
            (1 << ACTION_TICKET_SHIFT) | 0x4000 | HTCAPTION as LPARAM,
            (1 << ACTION_TICKET_SHIFT) | INPUT_DOUBLE_CLICK | HTRIGHT as LPARAM,
        ] {
            assert!(decode_action(action).is_none(), "accepted {action:#x}");
        }
        assert!(encode_action(0, HTCAPTION, false).is_none());
        assert!(encode_action(MAX_ACTION_TICKET + 1, HTCAPTION, false).is_none());
        assert!(encode_action(1, HTCLIENT, false).is_none());
        assert!(encode_action(1, HTCLOSE, true).is_none());
    }

    #[test]
    fn posting_failure_clears_only_the_failed_press() {
        let mut state = PressState::new();
        let press = begin(&mut state, 1, HTCAPTION);
        state.post_failed(1, press.action);
        assert!(state.claim_drag(1, press.action).is_none());
        assert!(state.release().is_none());
    }

    #[test]
    fn delayed_posting_failure_cannot_clear_a_newer_press() {
        let mut state = PressState::new();
        let old = begin(&mut state, 1, HTCAPTION);
        let current = begin(&mut state, 1, HTCAPTION);
        state.post_failed(1, old.action);
        assert!(state.claim_drag(1, current.action).is_some());
    }

    #[test]
    fn foreign_registration_failure_or_drop_cannot_clear_another_press() {
        let mut state = PressState::new();
        let press = begin(&mut state, 2, HTBOTTOMRIGHT);
        state.post_failed(1, press.action);
        state.cancel_registration(1);
        assert!(state.claim_drag(2, press.action).is_some());
    }

    #[test]
    fn container_removal_and_unrouted_new_down_invalidate_pending_actions() {
        let mut state = PressState::new();
        let removed = begin(&mut state, 1, HTCAPTION);
        state.cancel_registration(1);
        assert!(state.claim_drag(1, removed.action).is_none());
        let cancelled = begin(&mut state, 2, HTBOTTOM);
        state.cancel();
        assert!(state.claim_drag(2, cancelled.action).is_none());
        let invalidated = begin(&mut state, 2, HTLEFT);
        assert!(state
            .begin(2, HTCLIENT, false, POINT { x: 0, y: 0 }, 200)
            .is_none());
        assert!(state.claim_drag(2, invalidated.action).is_none());
    }

    #[test]
    fn ticket_rollover_skips_zero_and_still_decodes() {
        let mut state = PressState::new();
        state.next_ticket = MAX_ACTION_TICKET;
        let press = begin(&mut state, 1, HTCAPTION);
        assert_eq!((press.action as usize) >> ACTION_TICKET_SHIFT, 1);
        assert_eq!(decode_action(press.action), Some((HTCAPTION, false)));
    }

    #[test]
    fn invalid_registration_does_not_create_an_active_press() {
        let mut state = PressState::new();
        assert!(state
            .begin(0, HTCAPTION, false, POINT { x: 0, y: 0 }, 0)
            .is_none());
        assert!(state.release().is_none());
    }
}
