use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use crate::localization::tr;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PostMessageW, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

const CHECK_INTERVAL_MS: u64 = 16;
const ACTIVITY_SETTLE_MS: u64 = 400;
const MAX_BATCH_DURATION_MS: u64 = 1_000;
const MONITORED_EVENTS: [u32; 8] = [
    EVENT_SYSTEM_FOREGROUND,
    EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_MINIMIZEEND,
    EVENT_OBJECT_NAMECHANGE,
    EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE,
    EVENT_SYSTEM_MINIMIZESTART,
    EVENT_OBJECT_LOCATIONCHANGE,
];

#[derive(Clone, Copy)]
struct Target {
    token: usize,
    maintain: bool,
    container: bool,
}

#[derive(Default)]
struct PendingEvents {
    request_maintenance: bool,
    request_taskbar: bool,
    cancel_prior_checks: bool,
}

impl PendingEvents {
    fn record(&mut self, event: u32, maintain: bool) {
        match event {
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY | EVENT_SYSTEM_MINIMIZESTART => {
                // 取消标记保留到消费阶段，随后显示也必须先终止旧批次
                self.cancel_prior_checks = true;
                self.request_maintenance = false;
                self.request_taskbar = false;
            }
            EVENT_SYSTEM_FOREGROUND | EVENT_OBJECT_SHOW | EVENT_SYSTEM_MINIMIZEEND if maintain => {
                self.request_maintenance = true;
                self.request_taskbar |=
                    matches!(event, EVENT_OBJECT_SHOW | EVENT_SYSTEM_MINIMIZEEND);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct EventInbox {
    sink: usize,
    message: u32,
    targets: HashMap<usize, Target>,
    pending: HashMap<(usize, usize), PendingEvents>,
    notification_posted: bool,
}

thread_local! {
    static EVENTS: RefCell<EventInbox> = RefCell::new(EventInbox::default());
}

struct PendingCheck {
    token: usize,
    batch_started: Instant,
    next_check: Instant,
    ends_at: Instant,
    request_taskbar: bool,
}

impl PendingCheck {
    fn deadline(&self) -> Instant {
        self.next_check.min(self.ends_at)
    }
}

pub struct WindowEvents {
    hooks: Vec<HWINEVENTHOOK>,
    checks: HashMap<usize, PendingCheck>,
    _registration_thread: PhantomData<*mut ()>,
}

#[derive(Default)]
pub struct WindowEventUpdates {
    pub refresh_list: bool,
    pub has_window_events: bool,
    pub ready: Vec<(HWND, usize)>,
    pub taskbar: Vec<(HWND, usize)>,
}

impl WindowEventUpdates {
    fn queue_ready(&mut self, hwnd: usize, token: usize) {
        let target = (hwnd as HWND, token);
        if !self.ready.contains(&target) {
            self.ready.push(target);
        }
    }

    fn finish_batch(&mut self, hwnd: usize, check: &PendingCheck) {
        self.queue_ready(hwnd, check.token);
        self.refresh_list = true;
        if check.request_taskbar {
            self.taskbar.push((hwnd as HWND, check.token));
        }
    }
}

impl WindowEvents {
    pub unsafe fn new(sink: HWND, notification_message: u32) -> Result<Self, String> {
        if sink.is_null() {
            return Err(tr(
                "窗口事件监听需要有效的消息窗口",
                "Window event monitoring requires a valid message window",
            )
            .into());
        }
        let initialized = EVENTS.with(|events| {
            let Ok(mut events) = events.try_borrow_mut() else {
                return false;
            };
            if events.sink != 0 {
                return false;
            }
            *events = EventInbox {
                sink: sink as usize,
                message: notification_message,
                ..EventInbox::default()
            };
            true
        });
        if !initialized {
            return Err(tr(
                "当前线程已有窗口事件监听",
                "The current thread already has a window event monitor",
            )
            .into());
        }

        let mut monitor = Self {
            hooks: Vec::with_capacity(MONITORED_EVENTS.len()),
            checks: HashMap::new(),
            _registration_thread: PhantomData,
        };
        for event in MONITORED_EVENTS {
            let hook = SetWinEventHook(
                event,
                event,
                null_mut(),
                Some(window_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if hook.is_null() {
                return Err(tr(
                    "无法注册窗口事件监听",
                    "Could not register window event monitoring",
                )
                .into());
            }
            monitor.hooks.push(hook);
        }
        Ok(monitor)
    }

    pub fn set_targets(&mut self, targets: impl IntoIterator<Item = (HWND, usize, bool, bool)>) {
        let targets: HashMap<usize, Target> = targets
            .into_iter()
            .filter(|(hwnd, _, _, _)| !hwnd.is_null())
            .map(|(hwnd, token, maintain, container)| {
                (
                    hwnd as usize,
                    Target {
                        token,
                        maintain,
                        container,
                    },
                )
            })
            .collect();
        self.checks.retain(|hwnd, check| {
            targets.get(hwnd).is_some_and(|target| {
                target.token == check.token && target.maintain && !target.container
            })
        });
        EVENTS.with(|events| {
            let mut events = events.borrow_mut();
            events.pending.retain(|(hwnd, token), pending| {
                let Some(target) = targets.get(hwnd) else {
                    return false;
                };
                if target.token != *token {
                    return false;
                }
                if !target.maintain {
                    pending.request_maintenance = false;
                    pending.request_taskbar = false;
                }
                true
            });
            events.targets = targets;
        });
    }

    pub fn take_ready(&mut self) -> WindowEventUpdates {
        let pending = EVENTS.with(|events| {
            let mut events = events.borrow_mut();
            events.notification_posted = false;
            std::mem::take(&mut events.pending)
        });
        let mut updates = WindowEventUpdates {
            refresh_list: !pending.is_empty(),
            has_window_events: !pending.is_empty(),
            ready: Vec::new(),
            taskbar: Vec::new(),
        };
        let now = Instant::now();
        for ((hwnd, token), pending) in pending {
            let target = EVENTS.with(|events| {
                events
                    .borrow()
                    .targets
                    .get(&hwnd)
                    .filter(|target| target.token == token)
                    .copied()
            });
            let Some(target) = target else {
                self.checks.remove(&hwnd);
                continue;
            };
            if pending.cancel_prior_checks {
                self.checks.remove(&hwnd);
            }
            if !target.maintain || !pending.request_maintenance {
                continue;
            }
            if target.container {
                // 容器按当前事件同步状态，不建立透明度恢复或短时补检批次
                self.checks.remove(&hwnd);
                updates.queue_ready(hwnd, token);
                if pending.request_taskbar {
                    updates.taskbar.push((hwnd as HWND, token));
                }
                continue;
            }
            // 新活动先结算已到期批次，不能吞掉旧批次的最终任务栏维护
            if self
                .checks
                .get(&hwnd)
                .is_some_and(|check| check.token != token || check.ends_at <= now)
            {
                if let Some(check) = self.checks.remove(&hwnd) {
                    if check.token == token {
                        updates.finish_batch(hwnd, &check);
                    }
                }
            }
            // 每个新活动立即检查，后续短时补检可延长但整批不超过一秒
            self.checks
                .entry(hwnd)
                .and_modify(|check| {
                    check.next_check = now;
                    check.ends_at = (now + Duration::from_millis(ACTIVITY_SETTLE_MS))
                        .min(check.batch_started + Duration::from_millis(MAX_BATCH_DURATION_MS));
                    check.request_taskbar |= pending.request_taskbar;
                })
                .or_insert(PendingCheck {
                    token,
                    batch_started: now,
                    next_check: now,
                    ends_at: now + Duration::from_millis(ACTIVITY_SETTLE_MS),
                    request_taskbar: pending.request_taskbar,
                });
        }

        self.checks.retain(|hwnd, check| {
            if check.ends_at <= now {
                updates.finish_batch(*hwnd, check);
                return false;
            }
            if check.next_check <= now {
                updates.queue_ready(*hwnd, check.token);
                // 从本次消费时间安排下一次检查，消息阻塞后不补跑漏掉的检查
                check.next_check = now + Duration::from_millis(CHECK_INTERVAL_MS);
            }
            true
        });
        updates
    }

    pub fn next_delay_ms(&self) -> Option<u32> {
        let now = Instant::now();
        self.checks
            .values()
            .map(|check| {
                let duration = check.deadline().saturating_duration_since(now);
                // 向上取整，避免毫秒截断使定时器提前触发
                let milliseconds =
                    duration.as_millis() + u128::from(duration.subsec_nanos() % 1_000_000 != 0);
                milliseconds.clamp(1, u128::from(u32::MAX)) as u32
            })
            .min()
    }
}

impl Drop for WindowEvents {
    fn drop(&mut self) {
        // 先停止回调投递，再在注册线程释放 hooks
        EVENTS.with(|events| *events.borrow_mut() = EventInbox::default());
        for hook in self.hooks.drain(..) {
            unsafe {
                UnhookWinEvent(hook);
            }
        }
    }
}

unsafe extern "system" fn window_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if hwnd.is_null() || object != OBJID_WINDOW || child != 0 || !MONITORED_EVENTS.contains(&event)
    {
        return;
    }
    let notification = EVENTS.with(|events| {
        let Ok(mut events) = events.try_borrow_mut() else {
            return None;
        };
        if events.sink == 0 {
            return None;
        }
        let target = *events.targets.get(&(hwnd as usize))?;
        // 只有容器需要跟随目标的尺寸和最大化变化，普通窗口移动不触发同步
        if event == EVENT_OBJECT_LOCATIONCHANGE && !target.container {
            return None;
        }
        events
            .pending
            .entry((hwnd as usize, target.token))
            .or_default()
            .record(event, target.maintain);
        if events.notification_posted {
            return None;
        }
        events.notification_posted = true;
        Some((events.sink, events.message))
    });
    if let Some((sink, message)) = notification {
        // 释放 TLS 借用后再调用 Win32，防止可重入回调借用冲突
        if PostMessageW(sink as HWND, message, 0, 0) == 0 {
            EVENTS.with(|events| {
                if let Ok(mut events) = events.try_borrow_mut() {
                    if events.sink == sink && events.message == message {
                        events.notification_posted = false;
                    }
                }
            });
        }
    }
}
