//! On Windows, Quarry's header takes the place of the native title bar while the window keeps its native frame:
//! shadow, rounded corners, resizing from every edge and Aero Snap. The window procedure is subclassed to drop the
//! caption from the non-client area and to answer hit tests for the top resize band and the Maximize button, which
//! Windows 11 needs as HTMAXBUTTON to show its Snap Layouts flyout. Everything else in the header stays egui's:
//! dragging goes through ViewportCommand::StartDrag, which winit turns into the native move loop.
//!
//! The hit test is kept free of Win32 so it compiles and is tested on every platform.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// What a point in the client area is, for WM_NCHITTEST. `Pass` keeps the default answer, the client area.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Pass,
    Top,
    TopLeft,
    TopRight,
    Max,
}

/// `x`, `y` in client pixels, `w` the client width, `band` the resize band in pixels, `max` the Maximize button as
/// left, top, right, bottom in client pixels. A zoomed window has no resize band.
// Only the Windows window procedure calls it; the tests run it everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hit(x: i32, y: i32, w: i32, band: i32, zoomed: bool, max: [i32; 4]) -> Hit {
    if !zoomed && y >= 0 && y < band {
        // The corner bands reach as far along the top as the side borders reach down, like the native frame.
        return if x < band * 2 {
            Hit::TopLeft
        } else if x >= w - band * 2 {
            Hit::TopRight
        } else {
            Hit::Top
        };
    }
    let [l, t, r, b] = max;
    if x >= l && x < r && y >= t && y < b { Hit::Max } else { Hit::Pass }
}

// Shared between the egui frame, which draws the button and publishes where it is, and the window procedure, which
// sees the pointer there while egui does not. Both run on the main thread; atomics keep it free of locks.
// The rect is packed as four 16-bit edges in one atomic, so the window procedure never reads half of an old rect and half
// of a new one. Client pixels fit in 16 bits on any screen.
static MAX_RECT: AtomicU64 = AtomicU64::new(0);
static MAX_HOVER: AtomicBool = AtomicBool::new(false);
static MAX_PRESS: AtomicBool = AtomicBool::new(false);

/// Where the Maximize button is this frame, in client pixels.
pub fn set_max_rect(r: [i32; 4]) {
    let packed = r.iter().enumerate().fold(0u64, |acc, (i, &v)| acc | ((v.clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16 as u64) << (16 * i)));
    MAX_RECT.store(packed, Relaxed);
}

/// The rect `set_max_rect` last stored.
#[cfg_attr(not(windows), allow(dead_code))]
fn max_rect() -> [i32; 4] {
    let packed = MAX_RECT.load(Relaxed);
    std::array::from_fn(|i| (packed >> (16 * i)) as u16 as i16 as i32)
}

/// Whether the pointer is over the Maximize button, and whether it is pressed there, as the window procedure saw it.
pub fn max_state() -> (bool, bool) {
    (MAX_HOVER.load(Relaxed), MAX_PRESS.load(Relaxed))
}

#[cfg(windows)]
pub use native::install;

#[cfg(windows)]
mod native {
    use super::*;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicI32;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::Controls::MARGINS;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    const ID: usize = 0x5155_4152;
    /// `frame` at the window's DPI, 0 until it is first asked for and again after the DPI changes. Every pointer move asks for it.
    static FRAME: AtomicI32 = AtomicI32::new(0);

    fn repaint() {
        if let Some(f) = REPAINT.get() {
            f();
        }
    }

    /// The thickness of the sizing frame plus its padding at the window's DPI: the top resize band, and how far a
    /// maximized window hangs off the screen at the top.
    unsafe fn frame(hwnd: HWND) -> i32 {
        let cached = FRAME.load(Relaxed);
        if cached > 0 {
            return cached;
        }
        let f = unsafe {
            let dpi = GetDpiForWindow(hwnd);
            GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        };
        FRAME.store(f, Relaxed);
        f
    }

    unsafe fn client_h(hwnd: HWND) -> i32 {
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe { GetClientRect(hwnd, &mut r) };
        r.bottom - r.top
    }

    /// Subclasses the window so the header replaces the title bar. False leaves the native title bar as it was.
    /// `repaint` asks egui for a frame when the Maximize button's hover or press changes.
    pub fn install(hwnd: isize, repaint: impl Fn() + Send + Sync + 'static) -> bool {
        let hwnd = hwnd as HWND;
        let _ = REPAINT.set(Box::new(repaint));
        unsafe {
            if SetWindowSubclass(hwnd, Some(proc), ID, 0) == 0 {
                return false;
            }
            let before = client_h(hwnd);
            // A sliver of DWM frame inside the client area keeps the drop shadow on Windows 10. The header paints
            // over it, so it does not show.
            let m = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
            DwmExtendFrameIntoClientArea(hwnd, &m);
            SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            // The window was sized for the client area under a caption. Without the caption the client area grows by
            // its height, and the saved size would grow again on every launch.
            let grown = client_h(hwnd) - before;
            if grown > 0 && IsZoomed(hwnd) == 0 {
                let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetWindowRect(hwnd, &mut r);
                SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, r.right - r.left, r.bottom - r.top - grown, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        true
    }

    fn set_hover(on: bool, press: Option<bool>) {
        let mut changed = MAX_HOVER.swap(on, Relaxed) != on;
        if let Some(p) = press {
            changed |= MAX_PRESS.swap(p, Relaxed) != p;
        }
        if changed {
            repaint();
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
        unsafe {
            match msg {
                WM_NCCALCSIZE if wp != 0 => {
                    // Fullscreen drops the caption style and needs no change.
                    if GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_CAPTION != WS_CAPTION {
                        return DefSubclassProc(hwnd, msg, wp, lp);
                    }
                    let p = lp as *mut NCCALCSIZE_PARAMS;
                    let top = (*p).rgrc[0].top;
                    let r = DefSubclassProc(hwnd, msg, wp, lp);
                    // The sides and bottom keep their frame, which holds the resize borders and the shadow; only the
                    // caption and the top border go. A maximized window hangs its frame off the screen, so its client
                    // area starts that far down or the header would be cut off.
                    (*p).rgrc[0].top = top + if IsZoomed(hwnd) != 0 { frame(hwnd) } else { 0 };
                    r
                }
                WM_NCHITTEST => {
                    let def = DefSubclassProc(hwnd, msg, wp, lp);
                    if def != HTCLIENT as LRESULT {
                        return def;
                    }
                    // Signed, since screen coordinates go negative on a monitor left of or above the primary one.
                    let mut pt = POINT { x: (lp & 0xFFFF) as u16 as i16 as i32, y: ((lp >> 16) & 0xFFFF) as u16 as i16 as i32 };
                    ScreenToClient(hwnd, &mut pt);
                    let mut c = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetClientRect(hwnd, &mut c);
                    (match hit(pt.x, pt.y, c.right, frame(hwnd), IsZoomed(hwnd) != 0, max_rect()) {
                        Hit::Pass => HTCLIENT,
                        Hit::Top => HTTOP,
                        Hit::TopLeft => HTTOPLEFT,
                        Hit::TopRight => HTTOPRIGHT,
                        Hit::Max => HTMAXBUTTON,
                    }) as LRESULT
                }
                WM_NCMOUSEMOVE => {
                    set_hover(wp == HTMAXBUTTON as WPARAM, None);
                    // Re-armed on every move: Windows drops the request once it has sent one leave.
                    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
                    TrackMouseEvent(&mut t);
                    DefSubclassProc(hwnd, msg, wp, lp)
                }
                WM_NCMOUSELEAVE => {
                    set_hover(false, Some(false));
                    DefSubclassProc(hwnd, msg, wp, lp)
                }
                // Passed on, a press on HTMAXBUTTON makes Windows paint its classic button over the header and run
                // its own tracking loop. The click is ours instead: on release, if the press started there.
                WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wp == HTMAXBUTTON as WPARAM => {
                    set_hover(true, Some(true));
                    0
                }
                WM_NCLBUTTONUP if wp == HTMAXBUTTON as WPARAM => {
                    if MAX_PRESS.load(Relaxed) {
                        let cmd = if IsZoomed(hwnd) != 0 { SC_RESTORE } else { SC_MAXIMIZE };
                        PostMessageW(hwnd, WM_SYSCOMMAND, cmd as WPARAM, 0);
                    }
                    set_hover(true, Some(false));
                    0
                }
                WM_NCLBUTTONUP => {
                    set_hover(false, Some(false));
                    DefSubclassProc(hwnd, msg, wp, lp)
                }
                WM_DPICHANGED => {
                    FRAME.store(0, Relaxed);
                    DefSubclassProc(hwnd, msg, wp, lp)
                }
                WM_NCDESTROY => {
                    RemoveWindowSubclass(hwnd, Some(proc), ID);
                    DefSubclassProc(hwnd, msg, wp, lp)
                }
                _ => DefSubclassProc(hwnd, msg, wp, lp),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{HEADER_H, WIN_BUTTON_W};

    // A Maximize button as the header lays it out at 100 %.
    const MAX: [i32; 4] = [1000, 0, 1000 + WIN_BUTTON_W as i32, HEADER_H as i32];

    #[test]
    fn the_top_band_resizes_and_its_ends_take_the_corners() {
        assert_eq!(hit(500, 0, 1200, 8, false, MAX), Hit::Top);
        assert_eq!(hit(500, 7, 1200, 8, false, MAX), Hit::Top);
        assert_eq!(hit(500, 8, 1200, 8, false, MAX), Hit::Pass);
        assert_eq!(hit(3, 2, 1200, 8, false, MAX), Hit::TopLeft);
        assert_eq!(hit(1195, 2, 1200, 8, false, MAX), Hit::TopRight);
    }

    #[test]
    fn the_band_wins_over_the_maximize_button_at_the_top_edge() {
        assert_eq!(hit(1020, 3, 1200, 8, false, MAX), Hit::Top);
        assert_eq!(hit(1020, 20, 1200, 8, false, MAX), Hit::Max);
    }

    #[test]
    fn a_zoomed_window_has_no_band_and_the_button_reaches_the_top() {
        assert_eq!(hit(500, 0, 1200, 8, true, MAX), Hit::Pass);
        assert_eq!(hit(1020, 0, 1200, 8, true, MAX), Hit::Max);
    }

    #[test]
    fn the_button_rect_is_half_open_and_the_rest_stays_client() {
        assert_eq!(hit(1000, 20, 1200, 8, false, MAX), Hit::Max);
        assert_eq!(hit(1046, 20, 1200, 8, false, MAX), Hit::Pass);
        assert_eq!(hit(1020, 40, 1200, 8, false, MAX), Hit::Pass);
        assert_eq!(hit(1020, -5, 1200, 8, true, MAX), Hit::Pass);
        assert_eq!(hit(500, 400, 1200, 8, false, MAX), Hit::Pass);
    }

    #[test]
    fn the_published_rect_reads_back_whole() {
        set_max_rect([1000, -3, 1046, 40]);
        assert_eq!(max_rect(), [1000, -3, 1046, 40]);
        set_max_rect([0; 4]);
    }

    #[test]
    fn an_unpublished_button_never_hits() {
        assert_eq!(hit(0, 20, 1200, 8, false, [0; 4]), Hit::Pass);
    }
}
