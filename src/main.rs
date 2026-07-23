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
mod pill;
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

// Pill button ids, shared across both pill states.
const ID_MP4: u32 = 1;
const ID_GIF: u32 = 2;
const ID_REC: u32 = 3;
const ID_CANCEL: u32 = 4;
const ID_STOP: u32 = 5;
const ID_DISCARD: u32 = 6;

struct Recording {
    handle: record::RecorderHandle,
    started: std::time::Instant,
    region: (i32, i32, u32, u32),
    monitor: usize,
    // Kept alive for the recording's duration; windows die with the drop.
    _border: Option<picker::RecordBorder>,
}

struct GlimtApp {
    rx: mpsc::Receiver<AppMsg>,
    tray: tray::Tray,
    _hotkey: hotkey::Hotkey,
    settings: config::Settings,
    overlay: overlay::Overlay,
    picker: Option<picker::Picker>,
    pick_pill: Option<pill::Pill>,
    recording: Option<Recording>,
    rec_pill: Option<pill::Pill>,
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
                pick_pill: None,
                recording: None,
                rec_pill: None,
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
        self.picker_pill();
        self.recording_pill(&ctx);
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
        self.pick_pill = None;
        // The dim/border/pill windows must be off screen before the first frame
        // is captured, and DestroyWindow only takes effect at the next composition.
        dwm_flush();
        let format = self.settings.video_format;
        self.recording = Some(Recording {
            handle: record::start(region, format),
            started: std::time::Instant::now(),
            region,
            monitor,
            _border: picker::RecordBorder::show(region).ok(),
        });
        self.tray.set_recording(true);
    }

    /// Pre-record control pill under the picked region: size, MP4/GIF, Rec, cancel.
    fn picker_pill(&mut self) {
        let Some((region, monitor)) = self.picker.as_ref().and_then(|p| p.placed()) else {
            self.pick_pill = None;
            return;
        };
        let scale = self.overlay.scale_of(monitor);
        let (rx, ry, rw, rh) = region;
        let items = [
            pill::Item::Label(format!("{rw}\u{00D7}{rh}")),
            pill::Item::Sep,
            pill::Item::Button {
                id: ID_MP4,
                text: "MP4".into(),
                selected: self.settings.video_format == config::VideoFormat::Mp4,
            },
            pill::Item::Button {
                id: ID_GIF,
                text: "GIF".into(),
                selected: self.settings.video_format == config::VideoFormat::Gif,
            },
            pill::Item::Sep,
            pill::Item::Dot,
            pill::Item::Button {
                id: ID_REC,
                text: "Rec".into(),
                selected: false,
            },
            pill::Item::Button {
                id: ID_CANCEL,
                text: "Cancel".into(),
                selected: false,
            },
        ];

        if self.pick_pill.is_none() {
            // Never activatable: the picker's input window keeps the keyboard,
            // so Enter/Esc work even right after clicking the pill.
            self.pick_pill = pill::Pill::open(scale, false, None).ok();
        }
        let Some(p) = &self.pick_pill else { return };
        let (_, ph) = pill::Pill::measure(&items, scale);
        let (x, y) = pill_pos(self.overlay.monitor_rect(monitor), (rx, ry, rh), ph);
        p.set(x, y, &items);

        match p.take_click() {
            Some(ID_MP4) => self.set_format(config::VideoFormat::Mp4),
            Some(ID_GIF) => self.set_format(config::VideoFormat::Gif),
            Some(ID_REC) => self.start_picker_recording(),
            Some(ID_CANCEL) => {
                self.picker = None;
                self.pick_pill = None;
            }
            _ => {}
        }
    }

    fn set_format(&mut self, format: config::VideoFormat) {
        if self.settings.video_format != format {
            self.settings.video_format = format;
            self.settings.save();
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
        let Some(rec) = &self.recording else {
            self.rec_pill = None;
            return;
        };
        let scale = self.overlay.scale_of(rec.monitor);
        let (rx, ry, _rw, rh) = rec.region;
        let secs = rec.started.elapsed().as_secs();
        let items = [
            pill::Item::Dot,
            pill::Item::Label(format!("{}:{:02}", secs / 60, secs % 60)),
            pill::Item::Button {
                id: ID_STOP,
                text: "Stop".into(),
                selected: false,
            },
            pill::Item::Button {
                id: ID_DISCARD,
                text: "Discard".into(),
                selected: false,
            },
        ];

        if self.rec_pill.is_none() {
            // Activatable so Esc = discard works after a click; created without
            // stealing focus from whatever is being recorded.
            let p = pill::Pill::open(scale, true, Some(ID_DISCARD)).ok();
            if let Some(p) = &p {
                p.exclude_from_capture();
            }
            self.rec_pill = p;
        }
        let monitor_rect = self.overlay.monitor_rect(rec.monitor);
        if let Some(p) = &self.rec_pill {
            let (_, ph) = pill::Pill::measure(&items, scale);
            let (x, y) = pill_pos(monitor_rect, (rx, ry, rh), ph);
            p.set(x, y, &items);
            match p.take_click() {
                Some(ID_STOP) => rec.handle.stop(),
                Some(ID_DISCARD) => rec.handle.discard(),
                _ => {}
            }
        }
        // Keep the timer ticking (and the recorder channel polled).
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

/// Pill position under a region (physical px): left-aligned, flipped above the
/// region when it would land off the monitor's bottom.
fn pill_pos(
    monitor: (i32, i32, i32, i32),
    (rx, ry, rh): (i32, i32, u32),
    pill_h: i32,
) -> (i32, i32) {
    let (_, mon_y, _, mon_h) = monitor;
    let mut y = ry + rh as i32 + 8;
    if y + pill_h > mon_y + mon_h {
        y = ry - 8 - pill_h;
    }
    (rx, y)
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
