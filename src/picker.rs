//! Live region picker for recordings. Unlike the photo overlay (a frozen
//! screenshot in egui viewports), framing a recording needs the real, moving
//! screen visible. egui viewports can't provide that: transparent viewports
//! render as opaque black on this backend (see CLAUDE.md), so the picker is
//! raw Win32 windows instead — four half-transparent black layered "dim"
//! strips around the selection hole, four thin opaque amber border strips,
//! and one alpha=1 layered window spanning the whole virtual screen that is
//! invisible but hit-testable and owns all mouse/keyboard interaction.
//!
//! All geometry is in physical virtual-screen px (the process is PerMonitorV2,
//! so raw window coordinates are physical). Selections are clamped to the
//! monitor where the drag started, matching the photo overlay's behavior.

use std::cell::Cell;
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, CreateSolidBrush, GetStockObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CreateWindowExW, DefWindowProcW, DeferWindowPos, DestroyWindow,
    EndDeferWindowPos, GWLP_USERDATA, GetSystemMetrics, GetWindowLongPtrW, HWND_TOPMOST, IDC_CROSS,
    LWA_ALPHA, LoadCursorW, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

/// Alpha of the black dim strips over the live screen (0-255). Very light
/// (~15%): just enough to read as "picker active" without hiding the screen.
const DIM_ALPHA: u8 = 38;
const BORDER: i32 = 2; // border strip thickness, physical px
const AMBER: u32 = 0x003DC5FF; // COLORREF is 0x00BBGGRR: rgb(255, 197, 61)

pub enum Action {
    Start,
    Cancel,
}

#[derive(Clone, Copy)]
enum Sel {
    None,
    Dragging { start: (i32, i32), cur: (i32, i32) },
    Placed { rect: (i32, i32, i32, i32) }, // x, y, w, h
}

struct State {
    sel: Sel,
    monitor: Option<usize>,
    action: Option<Action>,
}

struct Shared {
    monitors: Vec<(i32, i32, i32, i32)>,
    virt: (i32, i32, i32, i32),
    dim: [Cell<isize>; 4],
    border: [Cell<isize>; 4],
    input: Cell<isize>,
    state: Mutex<State>,
}

pub struct Picker {
    // Boxed so the wndproc's GWLP_USERDATA pointer stays valid while windows live.
    shared: Box<Shared>,
}

impl Picker {
    pub fn open() -> Result<Picker> {
        let monitors = crate::capture::monitor_rects();
        if monitors.is_empty() {
            bail!("no monitors found");
        }
        let virt = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let shared = Box::new(Shared {
            monitors,
            virt,
            dim: Default::default(),
            border: Default::default(),
            input: Cell::new(0),
            state: Mutex::new(State {
                sel: Sel::None,
                monitor: None,
                action: None,
            }),
        });

        unsafe {
            let hinst = GetModuleHandleW(None)
                .context("GetModuleHandleW failed")?
                .into();
            register_classes(hinst);

            for cell in &shared.dim {
                let hwnd = create(hinst, w!("GlimtPickerDim"), true)?;
                SetLayeredWindowAttributes(hwnd, COLORREF(0), DIM_ALPHA, LWA_ALPHA)
                    .context("dim alpha")?;
                cell.set(hwnd.0 as isize);
            }
            for cell in &shared.border {
                cell.set(create(hinst, w!("GlimtPickerBorder"), false)?.0 as isize);
            }
            let input = create(hinst, w!("GlimtPickerDim"), true)?;
            // Alpha 1: invisible in practice, but unlike alpha 0 the window still
            // hit-tests, so it can own the crosshair drag over the live screen.
            SetLayeredWindowAttributes(input, COLORREF(0), 1, LWA_ALPHA).context("input alpha")?;
            make_interactive(input);
            shared.input.set(input.0 as isize);
            SetWindowLongPtrW(input, GWLP_USERDATA, &*shared as *const Shared as isize);

            let (vx, vy, vw, vh) = virt;
            SetWindowPos(input, Some(HWND_TOPMOST), vx, vy, vw, vh, SWP_SHOWWINDOW)
                .context("show input window")?;
            relayout(&shared);
            let _ = ShowWindow(input, SW_SHOW);
            // Allowed here because the global hotkey press granted this process
            // foreground rights; needed so Esc/Enter reach the input window.
            let _ = SetForegroundWindow(input);
            let _ = SetFocus(Some(input));
        }
        Ok(Picker { shared })
    }

    /// Placed selection as (x, y, w, h) virtual-screen physical px + monitor
    /// index. Dimensions clamped even for the H.264 encoder, like the overlay did.
    pub fn placed(&self) -> Option<((i32, i32, u32, u32), usize)> {
        let state = self.shared.state.lock().unwrap();
        let Sel::Placed { rect: (x, y, w, h) } = state.sel else {
            return None;
        };
        let region = (x, y, ((w as u32) & !1).max(2), ((h as u32) & !1).max(2));
        Some((region, state.monitor?))
    }

    pub fn take_action(&self) -> Option<Action> {
        self.shared.state.lock().unwrap().action.take()
    }
}

impl Drop for Picker {
    fn drop(&mut self) {
        let all = self
            .shared
            .dim
            .iter()
            .chain(&self.shared.border)
            .chain(std::iter::once(&self.shared.input));
        for cell in all {
            let hwnd = HWND(cell.get() as *mut _);
            if !hwnd.is_invalid() {
                unsafe {
                    // Clear the userdata pointer before the box can die.
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

unsafe fn create(
    hinst: windows::Win32::Foundation::HINSTANCE,
    class: windows::core::PCWSTR,
    layered: bool,
) -> Result<HWND> {
    let mut ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    if layered {
        ex |= WS_EX_LAYERED;
    }
    // Dim/border strips never take input; the fullscreen input window is
    // distinguished later by receiving the GWLP_USERDATA pointer.
    let hwnd = unsafe {
        CreateWindowExW(
            ex | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            class,
            w!("Glimt Picker"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
    }
    .context("CreateWindowExW failed")?;
    unsafe {
        let disable: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &disable as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
    Ok(hwnd)
}

fn register_classes(hinst: windows::Win32::Foundation::HINSTANCE) {
    unsafe {
        // Class brushes do the painting: no WM_PAINT handling needed anywhere.
        // Registration fails with "class already exists" on every open after
        // the first; that's fine.
        let dim = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: w!("GlimtPickerDim"),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&dim);
        let border = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: w!("GlimtPickerBorder"),
            hbrBackground: CreateSolidBrush(COLORREF(AMBER)),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        let _ = RegisterClassW(&border);
    }
}

/// The input window must NOT be click-through, unlike the strips; strip flags
/// are set at creation, so only the input window's WS_EX_TRANSPARENT and
/// WS_EX_NOACTIVATE need removing once it exists.
unsafe fn make_interactive(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongW, SetWindowLongW};
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let cleared = ex & !((WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as i32);
        SetWindowLongW(hwnd, GWL_EXSTYLE, cleared);
    }
}

fn current_hole(state: &State) -> Option<(i32, i32, i32, i32)> {
    match state.sel {
        Sel::None => None,
        Sel::Dragging { start, cur } => {
            let x = start.0.min(cur.0);
            let y = start.1.min(cur.1);
            Some((x, y, (start.0 - cur.0).abs(), (start.1 - cur.1).abs()))
        }
        Sel::Placed { rect } => Some(rect),
    }
}

/// Reposition the dim strips around the hole (or cover everything when there
/// is none) and frame the hole with the border strips. Batched so a drag
/// updates all eight windows in one composition pass.
fn relayout(shared: &Shared) {
    let (vx, vy, vw, vh) = shared.virt;
    let hole = current_hole(&shared.state.lock().unwrap());

    let (dim_rects, border_rects) = match hole {
        Some((x, y, w, h)) if w > 0 && h > 0 => (
            [
                (vx, vy, vw, y - vy),               // above
                (vx, y + h, vw, vy + vh - (y + h)), // below
                (vx, y, x - vx, h),                 // left
                (x + w, y, vx + vw - (x + w), h),   // right
            ],
            [
                // Horizontal strips overhang by BORDER to close the corners.
                (x - BORDER, y - BORDER, w + 2 * BORDER, BORDER), // top
                (x - BORDER, y + h, w + 2 * BORDER, BORDER),      // bottom
                (x - BORDER, y, BORDER, h),                       // left
                (x + w, y, BORDER, h),                            // right
            ],
        ),
        _ => (
            [(vx, vy, vw, vh), (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0)],
            [(0, 0, 0, 0); 4],
        ),
    };

    unsafe {
        let Ok(mut hdwp) = BeginDeferWindowPos(8) else {
            return;
        };
        let windows = shared
            .dim
            .iter()
            .zip(dim_rects)
            .chain(shared.border.iter().zip(border_rects));
        for (cell, (x, y, w, h)) in windows {
            let hwnd = HWND(cell.get() as *mut _);
            let vis = if w > 0 && h > 0 {
                SWP_SHOWWINDOW
            } else {
                SWP_HIDEWINDOW
            };
            match DeferWindowPos(
                hdwp,
                hwnd,
                None,
                x,
                y,
                w.max(0),
                h.max(0),
                vis | SWP_NOZORDER | SWP_NOACTIVATE,
            ) {
                Ok(next) => hdwp = next,
                Err(_) => return,
            }
        }
        let _ = EndDeferWindowPos(hdwp);
    }
}

fn monitor_at(shared: &Shared, p: (i32, i32)) -> usize {
    shared
        .monitors
        .iter()
        .position(|&(x, y, w, h)| p.0 >= x && p.0 < x + w && p.1 >= y && p.1 < y + h)
        .unwrap_or(0)
}

fn clamp_to_monitor(shared: &Shared, monitor: usize, p: (i32, i32)) -> (i32, i32) {
    let (x, y, w, h) = shared.monitors[monitor];
    (p.0.clamp(x, x + w), p.1.clamp(y, y + h))
}

/// Amber frame around a recorded region: four raw opaque strip windows just
/// outside it, excluded from capture. Raw Win32 rather than egui viewports —
/// the OS clamps a 2px-tall viewport window's height at creation (the old
/// egui top strip came out ~39px tall), and raw windows land on exact
/// physical px.
pub struct RecordBorder {
    hwnds: [isize; 4],
}

impl RecordBorder {
    pub fn show(region: (i32, i32, u32, u32)) -> Result<RecordBorder> {
        use windows::Win32::UI::WindowsAndMessaging::{
            SWP_SHOWWINDOW, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        };
        const T: i32 = BORDER;
        let (x, y, w, h) = (region.0, region.1, region.2 as i32, region.3 as i32);
        let rects = [
            // Horizontal strips overhang by T on both sides to close the corners.
            (x - T, y - T, w + 2 * T, T), // top
            (x - T, y + h, w + 2 * T, T), // bottom
            (x - T, y, T, h),             // left
            (x + w, y, T, h),             // right
        ];
        let mut hwnds = [0isize; 4];
        unsafe {
            let hinst = GetModuleHandleW(None)
                .context("GetModuleHandleW failed")?
                .into();
            register_classes(hinst);
            for (i, (rx, ry, rw, rh)) in rects.into_iter().enumerate() {
                let hwnd = create(hinst, w!("GlimtPickerBorder"), false)?;
                // The strips sit outside the recorded rect, but exclude them
                // from capture anyway so px rounding can't leak amber in.
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    rx,
                    ry,
                    rw,
                    rh,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
                hwnds[i] = hwnd.0 as isize;
            }
        }
        Ok(RecordBorder { hwnds })
    }
}

impl Drop for RecordBorder {
    fn drop(&mut self) {
        for hwnd in self.hwnds {
            let hwnd = HWND(hwnd as *mut _);
            if !hwnd.is_invalid() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Shared;
    let Some(shared) = (unsafe { ptr.as_ref() }) else {
        // Dim/border strips (and the input window before setup) have no state.
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    };

    // Screen coords: the input window's client origin IS the virtual-screen origin.
    let point = || {
        let x = (l.0 & 0xFFFF) as u16 as i16 as i32 + shared.virt.0;
        let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32 + shared.virt.1;
        (x, y)
    };

    match msg {
        WM_SETCURSOR => {
            unsafe {
                let _ = SetCursor(LoadCursorW(None, IDC_CROSS).ok());
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let p = point();
            let mut state = shared.state.lock().unwrap();
            state.monitor = Some(monitor_at(shared, p));
            state.sel = Sel::Dragging { start: p, cur: p };
            drop(state);
            unsafe { SetCapture(hwnd) };
            relayout(shared);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut state = shared.state.lock().unwrap();
            let monitor = state.monitor;
            if let (Sel::Dragging { cur, .. }, Some(mon)) = (&mut state.sel, monitor) {
                *cur = clamp_to_monitor(shared, mon, point());
                drop(state);
                relayout(shared);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let mut state = shared.state.lock().unwrap();
            if let Sel::Dragging { .. } = state.sel {
                match current_hole(&state) {
                    // A stray click (no real drag) clears back to full dim.
                    Some((x, y, w, h)) if w >= 4 && h >= 4 => {
                        state.sel = Sel::Placed { rect: (x, y, w, h) };
                    }
                    _ => state.sel = Sel::None,
                }
                drop(state);
                relayout(shared);
                crate::request_repaint(); // the pre-record pill appears/moves
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            shared.state.lock().unwrap().action = Some(Action::Cancel);
            crate::request_repaint();
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = w.0 as u16;
            let mut state = shared.state.lock().unwrap();
            if vk == VK_ESCAPE.0 {
                state.action = Some(Action::Cancel);
                crate::request_repaint();
            } else if vk == VK_RETURN.0 && matches!(state.sel, Sel::Placed { .. }) {
                state.action = Some(Action::Start);
                crate::request_repaint();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
