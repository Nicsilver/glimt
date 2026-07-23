#![windows_subsystem = "windows"]

mod autostart;
mod capture;
mod config;
mod encode_gif;
mod encode_mp4;
mod export;
mod hotkey;
mod overlay;
mod picker;
mod record;
mod single_instance;
mod tray;

use std::sync::mpsc;

use capture::ScreenCapturer;
use egui::ViewportBuilder;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

enum AppMsg {
    Capture { video: bool },
    OpenFolder,
    ToggleAutostart,
    Quit,
}

struct Recording {
    handle: record::RecorderHandle,
    started: std::time::Instant,
    region: (i32, i32, u32, u32),
    monitor: usize,
    affinity_set: bool,
    border_affinity_set: bool,
}

struct GlimtApp {
    rx: mpsc::Receiver<AppMsg>,
    tray: tray::Tray,
    _hotkey: hotkey::Hotkey,
    settings: config::Settings,
    overlay: overlay::Overlay,
    picker: Option<picker::Picker>,
    recording: Option<Recording>,
    // Run a few frames at startup so the overlay windows get created and their
    // per-monitor scales measured before the first capture.
    warmup_frames: u8,
}

fn main() {
    let Some(_guard) = single_instance::acquire() else {
        return; // another instance is running
    };

    let mut settings = config::Settings::load();
    autostart::sync(settings.autostart);
    hotkey::warn_if_snipping_owns_prtsc(&mut settings);

    // Tray + hotkey must be created on the thread that runs the win32 message
    // loop — eframe's main-thread loop provides it once run_native starts.
    let tray = match tray::build(settings.autostart) {
        Ok(t) => t,
        Err(e) => fatal(&format!("Failed to create tray icon: {e:#}")),
    };
    let hk = match hotkey::register_prtsc() {
        Ok(h) => h,
        Err(e) => fatal(&format!("Failed to set up hotkeys: {e:#}")),
    };

    let (tx, rx) = mpsc::channel::<AppMsg>();
    pump_events(tx, &tray, &hk);

    // The root window only keeps the event loop alive. It must stay technically
    // visible: eframe paints invisible windows outside the event-loop context,
    // which breaks spawning the overlay viewports (immediate viewports can't
    // create their windows there). So park a 1x1 undecorated tool window far
    // offscreen instead of hiding it.
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_position(egui::pos2(-30000.0, -30000.0))
            .with_inner_size(egui::vec2(1.0, 1.0))
            .with_decorations(false)
            .with_taskbar(false)
            .with_active(false),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Glimt",
        options,
        Box::new(move |cc| {
            // egui's browser-style Ctrl+/-/0 UI zoom changes pixels_per_point,
            // which the overlay math treats as the monitor scale — one stray
            // Ctrl+- makes every overlay window overflow its monitor (frozen
            // image shows cropped + upscaled) for the tray process's lifetime.
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            Ok(Box::new(GlimtApp {
                rx,
                tray,
                _hotkey: hk,
                settings,
                overlay: overlay::Overlay::new(),
                picker: None,
                recording: None,
                warmup_frames: 0,
            }))
        }),
    );
    if let Err(e) = result {
        fatal(&format!("UI failed to start: {e}"));
    }
}

/// Forward tray/menu/hotkey events into the UI thread's channel and wake it.
fn pump_events(tx: mpsc::Sender<AppMsg>, tray: &tray::Tray, hk: &hotkey::Hotkey) {
    let capture_id = tray.capture_id.clone();
    let open_folder_id = tray.open_folder_id.clone();
    let autostart_id = tray.autostart_item.id().clone();
    let quit_id = tray.quit_id.clone();

    let repaint: std::sync::Arc<std::sync::OnceLock<egui::Context>> = Default::default();
    REPAINT.set(repaint.clone()).ok();

    let wake = |repaint: &std::sync::Arc<std::sync::OnceLock<egui::Context>>| {
        if let Some(ctx) = repaint.get() {
            ctx.request_repaint();
        }
    };

    {
        let tx = tx.clone();
        let repaint = repaint.clone();
        let (prtsc_id, shift_prtsc_id) = (hk.prtsc_id, hk.shift_prtsc_id);
        std::thread::spawn(move || {
            for event in GlobalHotKeyEvent::receiver() {
                if event.state == HotKeyState::Pressed {
                    let video = match event.id {
                        id if id == prtsc_id => false,
                        id if id == shift_prtsc_id => true,
                        _ => continue,
                    };
                    let _ = tx.send(AppMsg::Capture { video });
                    wake(&repaint);
                }
            }
        });
    }
    {
        let tx = tx.clone();
        let repaint = repaint.clone();
        std::thread::spawn(move || {
            for event in TrayIconEvent::receiver() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let _ = tx.send(AppMsg::Capture { video: false });
                    wake(&repaint);
                }
            }
        });
    }
    std::thread::spawn(move || {
        for event in MenuEvent::receiver() {
            let msg = match &event.id {
                id if *id == capture_id => AppMsg::Capture { video: false },
                id if *id == open_folder_id => AppMsg::OpenFolder,
                id if *id == autostart_id => AppMsg::ToggleAutostart,
                id if *id == quit_id => AppMsg::Quit,
                _ => continue,
            };
            let _ = tx.send(msg);
            wake(&repaint);
        }
    });
}

// Set once the egui context exists so pump threads can request repaints.
static REPAINT: std::sync::OnceLock<std::sync::Arc<std::sync::OnceLock<egui::Context>>> =
    std::sync::OnceLock::new();

/// Wake the UI loop from outside it (picker wndproc, pump threads).
pub fn request_repaint() {
    if let Some(slot) = REPAINT.get()
        && let Some(ctx) = slot.get()
    {
        ctx.request_repaint();
    }
}

/// Block until DWM has composited pending window changes, so windows we just
/// destroyed or hid are really gone from the screen before a capture starts.
fn dwm_flush() {
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
    }
}

impl eframe::App for GlimtApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(slot) = REPAINT.get() {
            let _ = slot.set(ctx.clone());
        }

        if self.warmup_frames < 3 {
            self.warmup_frames += 1;
            ctx.request_repaint();
        }

        self.poll_recorder();

        if let Some(p) = &self.picker {
            match p.take_action() {
                Some(picker::Action::Cancel) => self.picker = None,
                Some(picker::Action::Start) => self.start_picker_recording(),
                None => {}
            }
        }

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::Capture { video } => {
                    if let Some(rec) = &self.recording {
                        // PrtSc (and friends) during a recording finish it.
                        rec.handle.stop();
                    } else if video {
                        if self.picker.is_some() {
                            self.picker = None; // Shift+PrtSc toggles the picker off
                        } else if !self.overlay.active() {
                            match picker::Picker::open() {
                                Ok(p) => self.picker = Some(p),
                                Err(e) => message_box(&format!("Record setup failed: {e:#}")),
                            }
                        }
                    } else if !self.overlay.active() {
                        if self.picker.take().is_some() {
                            // The dim strips are real windows: wait for DWM to
                            // composite their removal or they'd be in the shot.
                            dwm_flush();
                        }
                        match capture::GdiCapturer.capture_all() {
                            Ok(shots) => self.overlay.start(ctx, shots),
                            Err(e) => message_box(&format!("Capture failed: {e:#}")),
                        }
                    }
                }
                AppMsg::OpenFolder => {
                    if let Ok(dir) = config::save_dir() {
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }
                }
                AppMsg::ToggleAutostart => {
                    self.settings.autostart = !self.settings.autostart;
                    autostart::sync(self.settings.autostart);
                    self.settings.save();
                    self.tray
                        .autostart_item
                        .set_checked(self.settings.autostart);
                }
                AppMsg::Quit => {
                    // Drop the tray icon explicitly so it disappears before exit.
                    self.tray.icon.set_visible(false).ok();
                    std::process::exit(0);
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(outcome) = self.overlay.show_all(&ctx) {
            self.finish_overlay(&ctx, outcome);
        }
        self.picker_pill(&ctx);
        self.recording_pill(&ctx);
        self.record_border(&ctx);
    }
}

impl GlimtApp {
    fn finish_overlay(&mut self, ctx: &egui::Context, outcome: overlay::Outcome) {
        let export = || -> anyhow::Result<image::RgbaImage> {
            let (shot, sel, annotations) = self
                .overlay
                .export_data()
                .ok_or_else(|| anyhow::anyhow!("no selection"))?;
            export::render(shot, sel, annotations)
        };
        let result = match outcome {
            overlay::Outcome::Cancel => Ok(()),
            overlay::Outcome::Copy => export().and_then(|img| export::to_clipboard(&img)),
            overlay::Outcome::Save => export().map(|img| {
                let _ = export::to_file(&img);
            }),
        };
        self.overlay.close(ctx);
        if let Err(e) = result {
            message_box(&format!("Export failed: {e:#}"));
        }
    }

    /// Tear down the picker and start recording its selected region.
    fn start_picker_recording(&mut self) {
        let Some((region, monitor)) = self.picker.as_ref().and_then(|p| p.placed()) else {
            return;
        };
        self.picker = None;
        // The dim/border windows must be off screen before the first frame is
        // captured, and DestroyWindow only takes effect at the next composition.
        dwm_flush();
        let format = self.settings.video_format;
        self.recording = Some(Recording {
            handle: record::start(region, format),
            started: std::time::Instant::now(),
            region,
            monitor,
            affinity_set: false,
            border_affinity_set: false,
        });
        self.tray.set_recording(true);
    }

    /// Pre-record control pill under the picked region: size, MP4/GIF, Rec, cancel.
    fn picker_pill(&mut self, ctx: &egui::Context) {
        const PILL_SIZE: egui::Vec2 = egui::vec2(280.0, 44.0);
        let Some((region, monitor)) = self.picker.as_ref().and_then(|p| p.placed()) else {
            return;
        };
        let scale = self.overlay.scale_of(monitor);
        let (_, mon_y, _, mon_h) = self.overlay.monitor_rect(monitor);
        let (rx, ry, rw, rh) = region;

        // Physical px -> points; flip above the region if it would land off
        // the monitor's bottom (same logic as the recording pill).
        let pill_h_phys = (PILL_SIZE.y * scale) as i32;
        let mut y_phys = ry + rh as i32 + 8;
        if y_phys + pill_h_phys > mon_y + mon_h {
            y_phys = ry - 8 - pill_h_phys;
        }
        let pos = egui::pos2(rx as f32 / scale, y_phys as f32 / scale);

        let (mut start, mut cancel) = (false, false);
        let settings = &mut self.settings;
        let builder = ViewportBuilder::default()
            .with_title("Glimt Record")
            .with_position(pos)
            .with_inner_size(PILL_SIZE)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("glimt-pickbar"),
            builder,
            |ui, _| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 6.0, egui::Color32::from_rgb(27, 30, 40));
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!("{rw}\u{00D7}{rh}"))
                            .color(egui::Color32::GRAY)
                            .size(12.0),
                    );
                    ui.separator();
                    let formats = [
                        (config::VideoFormat::Mp4, "MP4"),
                        (config::VideoFormat::Gif, "GIF"),
                    ];
                    for (format, label) in formats {
                        if ui
                            .selectable_label(settings.video_format == format, label)
                            .clicked()
                        {
                            settings.video_format = format;
                            settings.save();
                        }
                    }
                    ui.separator();
                    // "●"/"✕" glyphs are tofu in egui's fonts; paint the red
                    // dot and use plain text instead.
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        dot.center(),
                        5.0,
                        egui::Color32::from_rgb(229, 72, 77),
                    );
                    if ui.button("Rec").clicked() {
                        start = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                // The picker's input window normally holds focus, but after a
                // click on the pill these land here instead.
                ui.input(|i| {
                    if i.key_pressed(egui::Key::Escape) {
                        cancel = true;
                    }
                    if i.key_pressed(egui::Key::Enter) {
                        start = true;
                    }
                });
            },
        );

        if cancel {
            self.picker = None;
        } else if start {
            self.start_picker_recording();
        }
    }

    fn poll_recorder(&mut self) {
        let Some(rec) = &self.recording else {
            return;
        };
        let Ok(msg) = rec.handle.rx.try_recv() else {
            return;
        };
        match msg {
            record::RecorderMsg::Done(path) => {
                // The file is saved either way; clipboard failure is only a warning.
                if let Err(e) = export::file_to_clipboard(&path) {
                    message_box(&format!("Saved, but copying to clipboard failed: {e:#}"));
                }
            }
            record::RecorderMsg::Discarded => {}
            record::RecorderMsg::Failed(e) => message_box(&format!("Recording failed: {e}")),
        }
        self.recording = None;
        self.tray.set_recording(false);
    }

    /// Floating always-on-top control pill next to the recorded region: timer,
    /// Stop, Discard. Excluded from capture so it never shows in the recording.
    fn recording_pill(&mut self, ctx: &egui::Context) {
        const PILL_SIZE: egui::Vec2 = egui::vec2(190.0, 44.0);
        let Some(rec) = &self.recording else {
            return;
        };
        let scale = self.overlay.scale_of(rec.monitor);
        let (_, mon_y, _, mon_h) = self.overlay.monitor_rect(rec.monitor);
        let (rx, ry, _rw, rh) = rec.region;
        let started = rec.started;

        // Physical px -> points; flip above the region if the pill would land
        // off the monitor's bottom (mirrors the toolbar's flip logic).
        let pill_h_phys = (PILL_SIZE.y * scale) as i32;
        let mut y_phys = ry + rh as i32 + 8;
        if y_phys + pill_h_phys > mon_y + mon_h {
            y_phys = ry - 8 - pill_h_phys;
        }
        let pos = egui::pos2(rx as f32 / scale, y_phys as f32 / scale);

        let (mut stop, mut discard) = (false, false);
        let builder = ViewportBuilder::default()
            .with_title("Glimt Recording")
            .with_position(pos)
            .with_inner_size(PILL_SIZE)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("glimt-recbar"),
            builder,
            |ui, _| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 6.0, egui::Color32::from_rgb(27, 30, 40));
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        dot.center(),
                        5.0,
                        egui::Color32::from_rgb(229, 72, 77),
                    );
                    let secs = started.elapsed().as_secs();
                    ui.label(
                        egui::RichText::new(format!("{}:{:02}", secs / 60, secs % 60))
                            .color(egui::Color32::WHITE),
                    );
                    if ui.button("Stop").clicked() {
                        stop = true;
                    }
                    if ui.button("Discard").clicked() {
                        discard = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    discard = true;
                }
            },
        );

        let rec = self.recording.as_mut().expect("recording checked above");
        if stop {
            rec.handle.stop();
        }
        if discard {
            rec.handle.discard();
        }
        if !rec.affinity_set && exclude_from_capture("Glimt Recording") {
            rec.affinity_set = true;
        }
        // Keep the timer ticking (and the recorder channel polled).
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }

    /// Amber frame around the region being recorded, built from four thin OPAQUE
    /// strip windows sitting just outside the region. One transparent overlay
    /// window is not an option: wgpu renders viewport transparency as an opaque
    /// black fill on Windows (confirmed on this machine).
    fn record_border(&mut self, ctx: &egui::Context) {
        const T: f32 = 2.0; // thickness in points
        let Some(rec) = &self.recording else {
            return;
        };
        let scale = self.overlay.scale_of(rec.monitor);
        let (rx, ry, rw, rh) = rec.region;
        let (x, y) = (rx as f32 / scale, ry as f32 / scale);
        let (w, h) = (rw as f32 / scale, rh as f32 / scale);
        // Horizontal strips overhang by T on both sides to close the corners.
        let strips = [
            (egui::pos2(x - T, y - T), egui::vec2(w + 2.0 * T, T)), // top
            (egui::pos2(x - T, y + h), egui::vec2(w + 2.0 * T, T)), // bottom
            (egui::pos2(x - T, y), egui::vec2(T, h)),               // left
            (egui::pos2(x + w, y), egui::vec2(T, h)),               // right
        ];
        for (i, (pos, size)) in strips.into_iter().enumerate() {
            let builder = ViewportBuilder::default()
                .with_title(format!("Glimt RecBorder{i}"))
                .with_position(pos)
                .with_inner_size(size)
                .with_decorations(false)
                .with_mouse_passthrough(true)
                .with_always_on_top()
                .with_taskbar(false)
                .with_resizable(false);
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("glimt-recborder", i)),
                builder,
                |ui, _| {
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        0.0,
                        egui::Color32::from_rgb(255, 197, 61),
                    );
                },
            );
        }
        let rec = self.recording.as_mut().expect("recording checked above");
        // The strips sit outside the recorded rect, but exclude them from capture
        // anyway so point->pixel rounding at fractional DPI can't leak an amber
        // line into the recording.
        if !rec.border_affinity_set {
            rec.border_affinity_set =
                (0..4).all(|i| exclude_from_capture(&format!("Glimt RecBorder{i}")));
        }
    }
}

/// Exclude this process's window with the given title from screen capture, so
/// the control pill never appears in the recording even when it overlaps it.
fn exclude_from_capture(title: &str) -> bool {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SetWindowDisplayAffinity,
        WDA_EXCLUDEFROMCAPTURE,
    };

    struct Data {
        title: Vec<u16>,
        done: bool,
    }
    unsafe extern "system" fn cb(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
        unsafe {
            let data = &mut *(data.0 as *mut Data);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                let mut buf = [0u16; 64];
                let len = GetWindowTextW(hwnd, &mut buf) as usize;
                if buf[..len] == data.title[..] {
                    data.done = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok();
                    return false.into(); // found it; stop enumerating
                }
            }
        }
        true.into()
    }
    let mut data = Data {
        title: title.encode_utf16().collect(),
        done: false,
    };
    unsafe {
        // Returns Err when the callback stops enumeration early; not a failure.
        let _ = EnumWindows(Some(cb), LPARAM(&mut data as *mut _ as isize));
    }
    data.done
}

fn message_box(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Glimt"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn fatal(text: &str) -> ! {
    message_box(text);
    std::process::exit(1);
}
