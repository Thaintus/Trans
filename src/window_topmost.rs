use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{GetLastError, SetLastError, HANDLE, HWND, LPARAM};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::localization::tr;
use crate::window_control::WindowInfo;

static PROPERTY: OnceLock<Vec<u16>> = OnceLock::new();
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);

pub(crate) struct Authority {
    hwnd: usize,
    process: u32,
    thread: u32,
    property: Vec<u16>,
    token: usize,
    restoring: bool,
}

impl Authority {
    pub(crate) fn new(window: &WindowInfo, property: &[u16], token: usize) -> Self {
        Self {
            hwnd: window.hwnd as usize,
            process: window.process_id,
            thread: window.thread_id,
            property: property.to_vec(),
            token,
            restoring: false,
        }
    }

    pub(crate) fn for_restore(window: &WindowInfo, property: &[u16], token: usize) -> Self {
        Self {
            restoring: true,
            ..Self::new(window, property, token)
        }
    }

    unsafe fn valid(&self) -> bool {
        let mut process = 0;
        let hwnd = self.hwnd as HWND;
        GetWindowThreadProcessId(hwnd, &mut process) == self.thread
            && process == self.process
            && GetPropW(hwnd, self.property.as_ptr()) as usize == self.token
    }
}

#[derive(Clone, Copy)]
struct Notice {
    sink: usize,
    message: u32,
    event: usize,
}

#[derive(Clone, Copy)]
struct Guard {
    hwnd: usize,
    process: u32,
    thread: u32,
    owner: usize,
    token: usize,
}

impl Guard {
    unsafe fn capture(hwnd: HWND, token: usize) -> Result<Self, String> {
        let mut process = 0;
        let thread = GetWindowThreadProcessId(hwnd, &mut process);
        if thread == 0 || process == 0 {
            return Err(tr("目标窗口已失效", "The target window is no longer valid").into());
        }
        if SetPropW(hwnd, property().as_ptr(), token as HANDLE) == 0 {
            return Err(api_error());
        }
        Ok(Self {
            hwnd: hwnd as usize,
            process,
            thread,
            owner: GetWindow(hwnd, GW_OWNER) as usize,
            token,
        })
    }

    unsafe fn valid(self) -> bool {
        let mut process = 0;
        let hwnd = self.hwnd as HWND;
        GetWindowThreadProcessId(hwnd, &mut process) == self.thread
            && process == self.process
            && GetWindow(hwnd, GW_OWNER) as usize == self.owner
            && GetPropW(hwnd, property().as_ptr()) as usize == self.token
    }

    unsafe fn clear(self) {
        let hwnd = self.hwnd as HWND;
        // 属性令牌区分同一线程复用的 HWND，不清理后来窗口的标记
        if GetPropW(hwnd, property().as_ptr()) as usize == self.token {
            RemovePropW(hwnd, property().as_ptr());
        }
    }
}

struct Job {
    target: Guard,
    allowed_owned: Option<Guard>,
    enabled: bool,
    receiver: Receiver<Result<(), String>>,
}

impl Job {
    fn contains(&self, hwnd: usize) -> bool {
        self.target.hwnd == hwnd || self.allowed_owned.is_some_and(|guard| guard.hwnd == hwnd)
    }

    unsafe fn clear(&self) {
        self.target.clear();
        if let Some(guard) = self.allowed_owned {
            guard.clear();
        }
    }
}

thread_local! {
    static NOTICE: Cell<Option<Notice>> = const { Cell::new(None) };
    static JOBS: RefCell<HashMap<usize, Job>> = RefCell::new(HashMap::new());
}

pub(crate) fn configure(sink: HWND, message: u32, event: usize) {
    NOTICE.with(|notice| {
        notice.set(Some(Notice {
            sink: sink as usize,
            message,
            event,
        }))
    });
}

fn property() -> &'static [u16] {
    PROPERTY.get_or_init(|| unsafe {
        format!("Trans.TopmostRequest.{}", GetCurrentProcessId())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    })
}

fn api_error() -> String {
    let code = unsafe { GetLastError() };
    format!(
        "{} {}",
        tr(
            "无法修改窗口置顶状态，错误码",
            "Cannot change always-on-top state; error"
        ),
        code
    )
}

pub(crate) fn pending_message() -> String {
    tr("置顶请求尚未完成，已保留窗口记录，等待目标程序响应后继续", "The always-on-top request is pending; the window record is retained until the target responds").into()
}

pub(crate) fn is_pending(hwnd: HWND) -> bool {
    JOBS.with(|jobs| {
        jobs.borrow()
            .values()
            .any(|job| job.contains(hwnd as usize))
    })
}

pub(crate) fn has_pending_requests() -> bool {
    JOBS.with(|jobs| !jobs.borrow().is_empty())
}

pub(crate) fn is_topmost(hwnd: HWND) -> Result<bool, String> {
    unsafe {
        SetLastError(0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if style == 0 && GetLastError() != 0 {
            return Err(api_error());
        }
        Ok(style & WS_EX_TOPMOST as isize != 0)
    }
}

// 仅接受能够独立恢复的窗口组合，避免误改未受管理的所属窗口状态
unsafe fn validate_group(
    hwnd: HWND,
    enabled: bool,
    allowed_owned: Option<HWND>,
    restoring: bool,
) -> Result<(), String> {
    let mut owner = GetWindow(hwnd, GW_OWNER);
    for _ in 0..64 {
        if owner.is_null() {
            break;
        }
        if allowed_owned != Some(owner) {
            return Err(tr(
                "该窗口具有外部所属关系，无法保证独立恢复置顶状态，请选择所属窗口操作",
                "This window has an external owner; select the owner so its topmost state can be restored independently",
            )
            .into());
        }
        owner = GetWindow(owner, GW_OWNER);
    }
    if !owner.is_null() {
        return Err(tr(
            "窗口所属关系过深，已取消置顶操作",
            "The ownership chain is too deep; the topmost change was cancelled",
        )
        .into());
    }
    // 最终恢复为置顶不会取消附属窗口原有置顶，无需套用新修改的回滚限制
    if !enabled || restoring {
        return Ok(());
    }
    struct Context {
        target: HWND,
        allowed: Option<HWND>,
        error: Option<String>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, value: LPARAM) -> i32 {
        let context = &mut *(value as *mut Context);
        if hwnd == context.target {
            return 1;
        }
        let mut owner = GetWindow(hwnd, GW_OWNER);
        let mut allowed = context.allowed == Some(hwnd);
        for _ in 0..64 {
            if owner.is_null() {
                return 1;
            }
            if owner == context.target {
                match is_topmost(hwnd) {
                    Ok(false) => return 1,
                    Ok(true) => {}
                    Err(error) => {
                        context.error = Some(error);
                        return 0;
                    }
                }
                if !allowed {
                    context.error = Some(tr("该窗口含有独立置顶的附属窗口，无法保证恢复原状态，已取消置顶操作", "This window has independently topmost owned windows; the change was cancelled to preserve their state").into());
                    return 0;
                }
                return 1;
            }
            allowed |= context.allowed == Some(owner);
            owner = GetWindow(owner, GW_OWNER);
        }
        context.error = Some(
            tr(
                "无法安全核对窗口所属关系",
                "Cannot safely verify window ownership",
            )
            .into(),
        );
        0
    }
    let allowed = match allowed_owned {
        Some(window) if is_topmost(window)? => Some(window),
        _ => None,
    };
    let mut context = Context {
        target: hwnd,
        allowed,
        error: None,
    };
    SetLastError(0);
    let enumerated = EnumWindows(Some(visit), &mut context as *mut Context as LPARAM);
    if let Some(error) = context.error {
        return Err(error);
    }
    if enumerated == 0 {
        return Err(api_error());
    }
    Ok(())
}

unsafe fn execute(
    target: Guard,
    allowed: Option<Guard>,
    enabled: bool,
    authority: &Authority,
) -> Result<(), String> {
    if !authority.valid() || !target.valid() || allowed.is_some_and(|guard| !guard.valid()) {
        return Err(tr(
            "目标窗口身份或所属关系已变化，已取消置顶操作",
            "The target identity or ownership changed; the topmost change was cancelled",
        )
        .into());
    }
    let hwnd = target.hwnd as HWND;
    validate_group(
        hwnd,
        enabled,
        allowed.map(|guard| guard.hwnd as HWND),
        authority.restoring,
    )?;
    if !authority.valid() || !target.valid() || allowed.is_some_and(|guard| !guard.valid()) {
        return Err(tr(
            "窗口身份或所属关系已变化，已停止修改",
            "The window identity or ownership changed; changes were stopped",
        )
        .into());
    }
    if SetWindowPos(
        hwnd,
        if enabled {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        },
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
    ) == 0
    {
        return Err(api_error());
    }
    if !authority.valid() || !target.valid() || allowed.is_some_and(|guard| !guard.valid()) {
        return Err(tr(
            "窗口身份已变化，请检查恢复记录",
            "The window identity changed; check the recovery record",
        )
        .into());
    }
    if is_topmost(hwnd)? != enabled {
        return Err(tr(
            "窗口未接受置顶状态修改，请重试",
            "The window did not accept the topmost change; try again",
        )
        .into());
    }
    Ok(())
}

pub(crate) fn request(
    hwnd: HWND,
    enabled: bool,
    allowed_owned: Option<HWND>,
    authority: Authority,
) -> Result<bool, String> {
    if !unsafe { authority.valid() } {
        return Err(tr(
            "目标窗口的管理标记已失效，已停止置顶操作",
            "The target's management marker is invalid; the topmost change was stopped",
        )
        .into());
    }
    let busy = JOBS.with(|jobs| {
        jobs.borrow()
            .values()
            .find(|job| {
                job.contains(hwnd as usize)
                    || allowed_owned.is_some_and(|window| job.contains(window as usize))
            })
            .map(|job| job.target.hwnd == hwnd as usize && job.enabled == enabled)
    });
    if let Some(same_request) = busy {
        return if same_request {
            Ok(false)
        } else {
            Err(pending_message())
        };
    }
    if is_topmost(hwnd)? == enabled {
        return Ok(true);
    }
    let notice = NOTICE.with(Cell::get).ok_or_else(|| {
        tr(
            "置顶通知尚未初始化",
            "Topmost notifications have not been initialized",
        )
        .to_string()
    })?;
    let token = loop {
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            break token;
        }
    };
    unsafe {
        validate_group(hwnd, enabled, allowed_owned, authority.restoring)?;
    }
    let target = unsafe { Guard::capture(hwnd, token)? };
    let allowed = match allowed_owned {
        Some(window) => match unsafe { Guard::capture(window, token) } {
            Ok(guard) => Some(guard),
            Err(error) => {
                unsafe {
                    target.clear();
                }
                return Err(error);
            }
        },
        None => None,
    };
    let (sender, receiver) = mpsc::channel();
    let language = crate::localization::current_language();
    let started = std::thread::Builder::new()
        .name("trans-topmost".into())
        .spawn(move || {
            crate::localization::set_language(language);
            let result = unsafe { execute(target, allowed, enabled, &authority) };
            let _ = sender.send(result);
            unsafe {
                PostMessageW(notice.sink as HWND, notice.message, notice.event, 0);
            }
        });
    if let Err(error) = started {
        unsafe {
            target.clear();
            if let Some(guard) = allowed {
                guard.clear();
            }
        }
        return Err(error.to_string());
    }
    JOBS.with(|jobs| {
        jobs.borrow_mut().insert(
            hwnd as usize,
            Job {
                target,
                allowed_owned: allowed,
                enabled,
                receiver,
            },
        )
    });
    Ok(false)
}

pub(crate) fn take_completed() -> Vec<(HWND, Result<(), String>)> {
    let completed = JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        let ready: Vec<_> = jobs
            .iter()
            .filter_map(|(key, job)| match job.receiver.try_recv() {
                Ok(result) => Some((*key, result)),
                Err(TryRecvError::Disconnected) => Some((
                    *key,
                    Err(tr(
                        "置顶处理线程已结束，请重试恢复",
                        "The topmost worker stopped; retry recovery",
                    )
                    .into()),
                )),
                Err(TryRecvError::Empty) => None,
            })
            .collect();
        ready
            .into_iter()
            .map(|(key, result)| (jobs.remove(&key).expect("已完成置顶请求"), result))
            .collect::<Vec<_>>()
    });
    completed
        .into_iter()
        .map(|(job, result)| {
            unsafe {
                job.clear();
            }
            (job.target.hwnd as HWND, result)
        })
        .collect()
}
