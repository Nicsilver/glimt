//! Software-rendered control pill with true per-pixel alpha.
//!
//! The egui pill viewports can't have smooth rounded corners: viewport
//! transparency renders as opaque black here (wgpu backend), and clipping the
//! window with SetWindowRgn is 1-bit, so the corners came out jagged. This
//! renders the pill into a premultiplied BGRA bitmap (tiny-skia shapes +
//! ab_glyph text, both already used by export.rs) and hands it to
//! UpdateLayeredWindow, so DWM alpha-blends the corners like any native
//! surface. Input is a plain wndproc: hit-tested clicks, hover repaints, and
//! the window never takes focus unless asked (so the picker keeps Esc/Enter).
//!
//! All geometry is in physical px; pass the monitor's scale for sizing.

use std::cell::Cell;
use std::sync::{Mutex, OnceLock};

use ab_glyph::{Font as _, FontRef, ScaleFont as _};
use anyhow::{Context as _, Result};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, IDC_ARROW,
    IDC_HAND, LoadCursorW, MA_NOACTIVATE, RegisterClassW, SW_SHOWNOACTIVATE, SetCursor,
    SetWindowDisplayAffinity, SetWindowLongPtrW, ShowWindow, ULW_ALPHA, UpdateLayeredWindow,
    WDA_EXCLUDEFROMCAPTURE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::w;

// Not exposed by the windows crate's WindowsAndMessaging module.
const WM_MOUSELEAVE: u32 = 0x02A3;

const FONT_BYTES: &[u8] = include_bytes!("../assets/Inter-Medium.ttf");

const BG: (u8, u8, u8, u8) = (27, 30, 40, 247);
const BG_BORDER: (u8, u8, u8, u8) = (255, 255, 255, 26);
const BTN: (u8, u8, u8, u8) = (45, 49, 63, 255);
const BTN_HOVER: (u8, u8, u8, u8) = (60, 66, 84, 255);
const BTN_SELECTED: (u8, u8, u8, u8) = (53, 116, 212, 255);
const TEXT: (u8, u8, u8, u8) = (230, 231, 235, 255);
const LABEL: (u8, u8, u8, u8) = (162, 168, 184, 255);
const SEP: (u8, u8, u8, u8) = (255, 255, 255, 26);
const DOT: (u8, u8, u8, u8) = (229, 72, 77, 255);

#[derive(Clone, PartialEq)]
pub enum Item {
    Label(String),
    Dot,
    Sep,
    Button { id: u32, text: String, selected: bool },
}

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(FONT_BYTES).expect("embedded font is valid"))
}

struct State {
    items: Vec<Item>,
    pos: (i32, i32),
    hit: Vec<((f32, f32, f32, f32), u32)>, // button rects (x, y, w, h) + id
    hover: Option<u32>,
    clicked: Option<u32>,
    shown: bool,
}

struct Shared {
    hwnd: Cell<isize>,
    scale: f32,
    // Only the recording pill activates on click (it wants Esc = discard);
    // the pre-record pill stays unfocusable so the picker keeps the keyboard.
    activatable: bool,
    esc_id: Option<u32>,
    state: Mutex<State>,
}

pub struct Pill {
    // Boxed so the wndproc's GWLP_USERDATA pointer stays valid while the window lives.
    shared: Box<Shared>,
}

impl Pill {
    pub fn open(scale: f32, activatable: bool, esc_id: Option<u32>) -> Result<Pill> {
        let shared = Box::new(Shared {
            hwnd: Cell::new(0),
            scale,
            activatable,
            esc_id,
            state: Mutex::new(State {
                items: Vec::new(),
                pos: (0, 0),
                hit: Vec::new(),
                hover: None,
                clicked: None,
                shown: false,
            }),
        });
        unsafe {
            let hinst = GetModuleHandleW(None).context("GetModuleHandleW failed")?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                lpszClassName: w!("GlimtPill"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            let _ = RegisterClassW(&class); // "already exists" after the first open
            let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
            if !activatable {
                ex |= WS_EX_NOACTIVATE;
            }
            let hwnd = CreateWindowExW(
                ex,
                w!("GlimtPill"),
                w!("Glimt Pill"),
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
            .context("CreateWindowExW failed")?;
            let disable: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &disable as *const i32 as *const _,
                std::mem::size_of::<i32>() as u32,
            );
            shared.hwnd.set(hwnd.0 as isize);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*shared as *const Shared as isize);
        }
        Ok(Pill { shared })
    }

    /// Rendered size for the given content, for positioning before `set`.
    pub fn measure(items: &[Item], scale: f32) -> (i32, i32) {
        let l = layout(items, scale);
        (l.w as i32, l.h as i32)
    }

    /// Move/update the pill; re-renders and re-presents only when something changed.
    pub fn set(&self, x: i32, y: i32, items: &[Item]) {
        let mut state = self.shared.state.lock().unwrap();
        let first = !state.shown;
        if !first && state.pos == (x, y) && state.items == items {
            return;
        }
        state.pos = (x, y);
        state.items = items.to_vec();
        present(&self.shared, &mut state);
        if first {
            state.shown = true;
            drop(state);
            unsafe {
                let _ = ShowWindow(self.hwnd(), SW_SHOWNOACTIVATE);
            }
        }
    }

    pub fn take_click(&self) -> Option<u32> {
        self.shared.state.lock().unwrap().clicked.take()
    }

    /// Keep this pill out of recordings/screenshots even when it overlaps them.
    pub fn exclude_from_capture(&self) {
        unsafe {
            let _ = SetWindowDisplayAffinity(self.hwnd(), WDA_EXCLUDEFROMCAPTURE);
        }
    }

    fn hwnd(&self) -> HWND {
        HWND(self.shared.hwnd.get() as *mut _)
    }
}

impl Drop for Pill {
    fn drop(&mut self) {
        let hwnd = self.hwnd();
        if !hwnd.is_invalid() {
            unsafe {
                // Clear the userdata pointer before the box can die.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

// ---------- layout ----------

enum Kind<'a> {
    Label(&'a str),
    Dot,
    Sep,
    Button { id: u32, text: &'a str, selected: bool },
}

struct Entry<'a> {
    kind: Kind<'a>,
    rect: (f32, f32, f32, f32), // x, y, w, h
}

struct Layout<'a> {
    w: f32,
    h: f32,
    entries: Vec<Entry<'a>>,
}

fn text_width(text: &str, px: f32) -> f32 {
    let scaled = font().as_scaled(ab_glyph::PxScale::from(px));
    let mut w = 0.0;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            w += scaled.kern(p, id);
        }
        w += scaled.h_advance(id);
        prev = Some(id);
    }
    w
}

fn layout<'a>(items: &'a [Item], s: f32) -> Layout<'a> {
    let h = (44.0 * s).round();
    let text_px = 14.0 * s;
    let pad = 12.0 * s;
    let gap = 8.0 * s;
    let btn_h = (26.0 * s).round();
    let btn_pad = 10.0 * s;

    let mut entries = Vec::new();
    let mut x = pad;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            x += gap;
        }
        let (kind, w, ih) = match item {
            Item::Label(t) => (Kind::Label(t), text_width(t, text_px), text_px * 1.4),
            Item::Dot => (Kind::Dot, 10.0 * s, 10.0 * s),
            Item::Sep => (Kind::Sep, 1.0_f32.max(s), 20.0 * s),
            Item::Button { id, text, selected } => (
                Kind::Button {
                    id: *id,
                    text,
                    selected: *selected,
                },
                (text_width(text, text_px) + btn_pad * 2.0).round(),
                btn_h,
            ),
        };
        entries.push(Entry {
            kind,
            rect: (x, ((h - ih) / 2.0).round(), w, ih),
        });
        x += w;
    }
    Layout {
        w: (x + pad).round(),
        h,
        entries,
    }
}

// ---------- rendering ----------

fn rgba(paint: &mut Paint, (r, g, b, a): (u8, u8, u8, u8)) {
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
}

fn rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    // Cubic approximation of quarter circles (kappa).
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * 0.552_285;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
}

fn fill_rounded(pixmap: &mut Pixmap, rect: (f32, f32, f32, f32), radius: f32, color: (u8, u8, u8, u8)) {
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, rect.0, rect.1, rect.2, rect.3, radius);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        rgba(&mut paint, color);
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Light-on-dark text blended linearly in sRGB reads thin; boosting the
/// coverage curve brightens the AA edge pixels back to perceptually-even
/// weight (egui applies the same kind of tweak internally).
const COVERAGE_GAMMA: f32 = 0.6;

/// Source-over glyph coverage onto the PREMULTIPLIED pixmap (export.rs's
/// version assumes an opaque background and ignores alpha).
fn draw_text(pixmap: &mut Pixmap, x: f32, baseline: f32, text: &str, px: f32, color: (u8, u8, u8, u8)) {
    let f = font();
    let scaled = f.as_scaled(ab_glyph::PxScale::from(px));
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();

    // Whole-pixel glyph placement: fractional positions smear the AA and the
    // text goes soft. Advances stay fractional so spacing remains correct.
    let baseline = baseline.round();
    let mut pen_x = x;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            pen_x += scaled.kern(p, id);
        }
        let glyph = id.with_scale_and_position(px, ab_glyph::point(pen_x.round(), baseline));
        if let Some(outlined) = f.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let dx = bounds.min.x as i32 + gx as i32;
                let dy = bounds.min.y as i32 + gy as i32;
                if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                    return;
                }
                let idx = ((dy * pw + dx) * 4) as usize;
                let a = cov.clamp(0.0, 1.0).powf(COVERAGE_GAMMA) * (color.3 as f32 / 255.0);
                let src = [color.0 as f32, color.1 as f32, color.2 as f32, 255.0];
                for c in 0..4 {
                    let dst = data[idx + c] as f32;
                    data[idx + c] = (src[c] * a + dst * (1.0 - a)).round().min(255.0) as u8;
                }
            });
        }
        pen_x += scaled.h_advance(id);
        prev = Some(id);
    }
}

fn render(items: &[Item], s: f32, hover: Option<u32>) -> (Pixmap, Vec<((f32, f32, f32, f32), u32)>) {
    let l = layout(items, s);
    let mut pixmap = Pixmap::new(l.w as u32, l.h as u32).expect("pill pixmap");
    let text_px = 14.0 * s;
    let scaled = font().as_scaled(ab_glyph::PxScale::from(text_px));
    let (ascent, descent) = (scaled.ascent(), scaled.descent());
    let baseline_in = |r: (f32, f32, f32, f32)| r.1 + r.3 / 2.0 + (ascent + descent) / 2.0;

    fill_rounded(&mut pixmap, (0.0, 0.0, l.w, l.h), 8.0 * s, BG);
    // Hairline inset border for definition against busy backgrounds.
    {
        let mut pb = PathBuilder::new();
        rounded_rect(&mut pb, 0.5, 0.5, l.w - 1.0, l.h - 1.0, 8.0 * s);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            rgba(&mut paint, BG_BORDER);
            let stroke = tiny_skia::Stroke {
                width: 1.0,
                ..Default::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }

    let mut hit = Vec::new();
    for e in &l.entries {
        match &e.kind {
            Kind::Label(t) => {
                draw_text(&mut pixmap, e.rect.0, baseline_in(e.rect), t, text_px, LABEL);
            }
            Kind::Dot => {
                let mut pb = PathBuilder::new();
                pb.push_circle(
                    e.rect.0 + e.rect.2 / 2.0,
                    e.rect.1 + e.rect.3 / 2.0,
                    e.rect.2 / 2.0,
                );
                if let Some(path) = pb.finish() {
                    let mut paint = Paint::default();
                    rgba(&mut paint, DOT);
                    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
            Kind::Sep => {
                fill_rounded(&mut pixmap, e.rect, 0.0, SEP);
            }
            Kind::Button { id, text, selected } => {
                let bg = if *selected {
                    BTN_SELECTED
                } else if hover == Some(*id) {
                    BTN_HOVER
                } else {
                    BTN
                };
                fill_rounded(&mut pixmap, e.rect, 5.0 * s, bg);
                let tx = e.rect.0 + (e.rect.2 - text_width(text, text_px)) / 2.0;
                draw_text(&mut pixmap, tx, baseline_in(e.rect), text, text_px, TEXT);
                hit.push((e.rect, *id));
            }
        }
    }
    (pixmap, hit)
}

/// Push the rendered bitmap to the layered window (position + size + pixels in one call).
fn present(shared: &Shared, state: &mut State) {
    let (pixmap, hit) = render(&state.items, shared.scale, state.hover);
    state.hit = hit;
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let hwnd = HWND(shared.hwnd.get() as *mut _);

    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            DeleteDC(mem_dc).ok().unwrap_or_default();
            ReleaseDC(None, screen_dc);
            return;
        };
        // tiny-skia is premultiplied RGBA; the DIB wants premultiplied BGRA.
        let src = pixmap.data();
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, src.len());
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        let old = SelectObject(mem_dc, dib.into());

        let pos = POINT {
            x: state.pos.0,
            y: state.pos.1,
        };
        let size = SIZE { cx: w, cy: h };
        let src_pos = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            Some(&pos),
            Some(&size),
            Some(mem_dc),
            Some(&src_pos),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
    }
}

// ---------- input ----------

fn hit_test(state: &State, x: f32, y: f32) -> Option<u32> {
    state
        .hit
        .iter()
        .find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
        .map(|(_, id)| *id)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Shared;
    let Some(shared) = (unsafe { ptr.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    };
    let point = || {
        let x = (l.0 & 0xFFFF) as u16 as i16 as f32;
        let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
        (x, y)
    };

    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = point();
            let mut state = shared.state.lock().unwrap();
            let hover = hit_test(&state, x, y);
            if hover != state.hover {
                state.hover = hover;
                present(shared, &mut state);
            }
            drop(state);
            unsafe {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let mut state = shared.state.lock().unwrap();
            if state.hover.is_some() {
                state.hover = None;
                present(shared, &mut state);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point();
            let mut state = shared.state.lock().unwrap();
            if let Some(id) = hit_test(&state, x, y) {
                state.clicked = Some(id);
                drop(state);
                crate::request_repaint();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let (Some(id), true) = (shared.esc_id, w.0 as u16 == VK_ESCAPE.0) {
                shared.state.lock().unwrap().clicked = Some(id);
                crate::request_repaint();
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE if !shared.activatable => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let cursor = unsafe {
                use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
                use windows::Win32::Graphics::Gdi::ScreenToClient;
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
                let state = shared.state.lock().unwrap();
                if hit_test(&state, p.x as f32, p.y as f32).is_some() {
                    IDC_HAND
                } else {
                    IDC_ARROW
                }
            };
            unsafe {
                let _ = SetCursor(LoadCursorW(None, cursor).ok());
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
