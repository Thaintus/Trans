use std::cell::Cell;

use windows_sys::Win32::Graphics::Gdi::{
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SWP_FRAMECHANGED, SWP_NOCOPYBITS, SWP_NOSIZE, SWP_SHOWWINDOW,
};

pub const FULL_REPAINT_FLAGS: u32 =
    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW;
const RESIZE_REPAINT_FLAGS: u32 = RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW;

pub fn position_affects_surface(flags: u32) -> bool {
    flags & SWP_NOSIZE == 0 || flags & (SWP_FRAMECHANGED | SWP_NOCOPYBITS | SWP_SHOWWINDOW) != 0
}

struct ActiveScope<'a> {
    active: &'a Cell<bool>,
    previous: bool,
}

impl<'a> ActiveScope<'a> {
    fn new(active: &'a Cell<bool>) -> Self {
        Self {
            active,
            previous: active.replace(true),
        }
    }
}

impl Drop for ActiveScope<'_> {
    fn drop(&mut self) {
        self.active.set(self.previous);
    }
}

// 画面失效独立于布局尺寸缓存，完整布局提交后同步绘制控件树
pub struct WindowRedraw {
    pending: Cell<bool>,
    deferred: Cell<bool>,
    painting: Cell<bool>,
    resizing: Cell<bool>,
}

impl WindowRedraw {
    pub const fn new() -> Self {
        Self {
            pending: Cell::new(false),
            deferred: Cell::new(false),
            painting: Cell::new(false),
            resizing: Cell::new(false),
        }
    }

    pub fn request(&self) {
        self.pending.set(true);
    }

    pub fn pending(&self) -> bool {
        self.pending.get()
    }

    pub fn clear(&self) {
        self.pending.set(false);
        self.resizing.set(false);
    }

    pub fn begin_resize(&self) {
        self.resizing.set(true);
    }

    pub fn end_resize(&self) {
        if self.resizing.replace(false) {
            self.request();
        }
    }

    pub fn defer<R>(&self, action: impl FnOnce() -> R) -> R {
        let _scope = ActiveScope::new(&self.deferred);
        action()
    }

    pub fn flush(&self, repaint: impl FnOnce(u32) -> bool) -> bool {
        if self.deferred.get() || self.painting.get() || !self.pending.replace(false) {
            return false;
        }
        let _scope = ActiveScope::new(&self.painting);
        // 交互期间同步呈现完整客户区，结束后再补绘非客户区
        let flags = if self.resizing.get() {
            RESIZE_REPAINT_FLAGS
        } else {
            FULL_REPAINT_FLAGS
        };
        let painted = repaint(flags);
        if !painted {
            self.request();
        }
        painted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER};

    // 模拟像素已丢失但原生失效区为空，不能把布局缓存当成画面有效性
    struct Surface {
        background: bool,
        caption: bool,
        buttons: [bool; 4],
        paints: usize,
    }

    impl Surface {
        fn discarded() -> Self {
            Self {
                background: false,
                caption: false,
                buttons: [false; 4],
                paints: 0,
            }
        }

        fn repaint(&mut self, flags: u32) -> bool {
            self.paints += 1;
            if flags & (RDW_INVALIDATE | RDW_UPDATENOW) == RDW_INVALIDATE | RDW_UPDATENOW {
                self.background = flags & RDW_ERASE != 0;
                self.caption = flags & RDW_FRAME != 0;
                self.buttons.fill(flags & RDW_ALLCHILDREN != 0);
            }
            true
        }

        fn complete(&self) -> bool {
            self.background && self.caption && self.buttons.iter().all(|&visible| visible)
        }
    }

    #[test]
    fn resizing_repaints_unchanged_buttons_even_when_layout_is_cached() {
        let redraw = WindowRedraw::new();
        let mut layout_size = None;
        let mut layouts = 0;
        for size in [(340, 364), (340, 364), (500, 500), (320, 300), (320, 300)] {
            let mut surface = Surface::discarded();
            redraw.defer(|| {
                if layout_size != Some(size) {
                    layout_size = Some(size);
                    layouts += 1;
                }
                redraw.request();
                redraw.request();
                assert!(!redraw.flush(|flags| surface.repaint(flags)));
                assert_eq!(surface.paints, 0);
            });
            assert!(redraw.flush(|flags| surface.repaint(flags)));
            assert!(surface.complete(), "buttons or frame missing at {size:?}");
            assert_eq!(surface.paints, 1);
            assert!(!redraw.pending());
        }
        assert_eq!(layouts, 3);
    }

    #[test]
    fn frame_change_and_discarded_pixels_require_redraw_without_a_size_change() {
        for flags in [
            SWP_NOSIZE | SWP_FRAMECHANGED,
            SWP_NOSIZE | SWP_NOCOPYBITS,
            SWP_NOSIZE | SWP_SHOWWINDOW,
            SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
        ] {
            assert!(position_affects_surface(flags));
        }
        for flags in [SWP_NOSIZE, SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE] {
            assert!(!position_affects_surface(flags));
        }
    }

    #[test]
    fn nested_position_updates_finish_before_a_single_repaint() {
        let redraw = WindowRedraw::new();
        redraw.defer(|| {
            redraw.request();
            redraw.defer(|| {
                redraw.request();
                assert!(!redraw.flush(|_| panic!("painted inside nested position update")));
            });
            assert!(!redraw.flush(|_| panic!("painted before outer position update finished")));
        });
        let paints = Cell::new(0);
        assert!(redraw.flush(|_| {
            paints.set(paints.get() + 1);
            true
        }));
        assert!(!redraw.flush(|_| panic!("painted unchanged surface twice")));
        assert_eq!(paints.get(), 1);
    }

    #[test]
    fn repaint_requested_inside_paint_survives_without_recursive_painting() {
        let redraw = WindowRedraw::new();
        redraw.request();
        assert!(redraw.flush(|_| {
            redraw.request();
            assert!(!redraw.flush(|_| panic!("recursive paint")));
            true
        }));
        assert!(redraw.pending());
        assert!(redraw.flush(|_| true));
        assert!(!redraw.pending());
    }

    #[test]
    fn failed_repaint_is_retained_for_a_later_dispatch_without_busy_retry() {
        let redraw = WindowRedraw::new();
        let attempts = Cell::new(0);
        redraw.request();
        assert!(!redraw.flush(|_| {
            attempts.set(attempts.get() + 1);
            false
        }));
        assert_eq!(attempts.get(), 1);
        assert!(redraw.pending());
        assert!(redraw.flush(|_| {
            attempts.set(attempts.get() + 1);
            true
        }));
        assert_eq!(attempts.get(), 2);
        assert!(!redraw.pending());
    }

    #[test]
    fn idle_and_shutdown_states_do_not_repaint() {
        let redraw = WindowRedraw::new();
        assert!(!redraw.flush(|_| panic!("paint without damage")));
        redraw.request();
        redraw.clear();
        assert!(!redraw.flush(|_| panic!("paint after shutdown")));
    }

    #[test]
    fn continuous_resize_presents_each_completed_layout_before_returning() {
        let redraw = WindowRedraw::new();
        let mut synchronous_paints = 0;
        redraw.begin_resize();
        for _ in 0..100 {
            redraw.request();
            assert!(redraw.flush(|flags| {
                assert_ne!(flags & RDW_UPDATENOW, 0);
                assert_eq!(flags & RDW_FRAME, 0);
                assert_ne!(flags & RDW_INVALIDATE, 0);
                assert_ne!(flags & RDW_ALLCHILDREN, 0);
                synchronous_paints += usize::from(flags & RDW_UPDATENOW != 0);
                true
            }));
        }
        assert_eq!(synchronous_paints, 100);
        redraw.end_resize();
        assert!(redraw.flush(|flags| {
            assert_eq!(flags, FULL_REPAINT_FLAGS);
            synchronous_paints += 1;
            true
        }));
        assert_eq!(synchronous_paints, 101);
        assert!(!redraw.flush(|_| panic!("duplicate final repaint")));
    }

    #[test]
    fn cancelled_or_cleared_resize_restores_normal_painting() {
        let redraw = WindowRedraw::new();
        for clear in [false, true] {
            redraw.begin_resize();
            redraw.request();
            if clear {
                redraw.clear();
                redraw.request();
            } else {
                redraw.end_resize();
            }
            assert!(redraw.flush(|flags| {
                assert_eq!(flags, FULL_REPAINT_FLAGS);
                true
            }));
        }
    }
}
