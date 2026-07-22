#![windows_subsystem = "windows"]

mod autostart;
mod capture;
mod config;
mod export;
mod hotkey;
mod overlay;
mod single_instance;
mod tray;

use std::sync::mpsc;

use capture::ScreenCapturer;
use egui::ViewportBuilder;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

enum AppMsg {
    Capture,
    OpenFolder,
    ToggleAutostart,
    Quit,
}

enum Mode {
    Hidden,
    Overlay(overlay::OverlayState),
}

struct GlimtApp {
    rx: mpsc::Receiver<AppMsg>,
    tray: tray::Tray,
    _hotkey: hotkey::Hotkey,
    settings: config::Settings,
    mode: Mode,
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
    pump_events(tx, &tray);

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
            let _ = &cc;
            Ok(Box::new(GlimtApp {
                rx,
                tray,
                _hotkey: hk,
                settings,
                mode: Mode::Hidden,
            }))
        }),
    );
    if let Err(e) = result {
        fatal(&format!("UI failed to start: {e}"));
    }
}

/// Forward tray/menu/hotkey events into the UI thread's channel and wake it.
fn pump_events(tx: mpsc::Sender<AppMsg>, tray: &tray::Tray) {
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
        std::thread::spawn(move || {
            for event in GlobalHotKeyEvent::receiver() {
                if event.state == HotKeyState::Pressed {
                    let _ = tx.send(AppMsg::Capture);
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
                    let _ = tx.send(AppMsg::Capture);
                    wake(&repaint);
                }
            }
        });
    }
    std::thread::spawn(move || {
        for event in MenuEvent::receiver() {
            let msg = match &event.id {
                id if *id == capture_id => AppMsg::Capture,
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

impl eframe::App for GlimtApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(slot) = REPAINT.get() {
            let _ = slot.set(ctx.clone());
        }

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::Capture => {
                    if matches!(self.mode, Mode::Hidden) {
                        match capture::GdiCapturer.capture_all() {
                            Ok(shots) => {
                                self.mode = Mode::Overlay(overlay::OverlayState::new(ctx, shots));
                            }
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
        if let Mode::Overlay(state) = &mut self.mode
            && let Some(outcome) = state.show_all(&ctx)
        {
            self.finish_overlay(outcome);
        }
    }
}

impl GlimtApp {
    fn finish_overlay(&mut self, outcome: overlay::Outcome) {
        let Mode::Overlay(state) = std::mem::replace(&mut self.mode, Mode::Hidden) else {
            return;
        };
        let export = |state: &overlay::OverlayState| -> anyhow::Result<image::RgbaImage> {
            let mon = state
                .active_monitor
                .ok_or_else(|| anyhow::anyhow!("no selection"))?;
            let sel = state
                .selection_rect()
                .ok_or_else(|| anyhow::anyhow!("no selection"))?;
            export::render(&state.shots[mon].image, sel, &state.annotations)
        };
        let result = match outcome {
            overlay::Outcome::Cancel => Ok(()),
            overlay::Outcome::Copy => export(&state).and_then(|img| export::to_clipboard(&img)),
            overlay::Outcome::Save => export(&state).map(|img| {
                let _ = export::to_file(&img);
            }),
        };
        if let Err(e) = result {
            message_box(&format!("Export failed: {e:#}"));
        }
    }
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
