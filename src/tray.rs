use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub struct Tray {
    // Kept alive for the process lifetime; dropping removes the tray icon.
    pub icon: TrayIcon,
    normal_icon: tray_icon::Icon,
    rec_icon: tray_icon::Icon,
    pub capture_id: tray_icon::menu::MenuId,
    pub open_folder_id: tray_icon::menu::MenuId,
    pub autostart_item: CheckMenuItem,
    pub quit_id: tray_icon::menu::MenuId,
}

impl Tray {
    /// Swap the tray icon to/from the red-dot recording variant.
    pub fn set_recording(&self, on: bool) {
        let icon = if on {
            &self.rec_icon
        } else {
            &self.normal_icon
        };
        let _ = self.icon.set_icon(Some(icon.clone()));
    }
}

fn load_icon(png: &[u8]) -> Result<tray_icon::Icon> {
    let rgba = image::load_from_memory(png)?.into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(tray_icon::Icon::from_rgba(rgba.into_raw(), w, h)?)
}

pub fn build(autostart_enabled: bool) -> Result<Tray> {
    let normal_icon = load_icon(include_bytes!("../assets/tray.png"))?;
    let rec_icon = load_icon(include_bytes!("../assets/tray-rec.png"))?;

    let capture = MenuItem::new("Capture (PrtSc)", true, None);
    let open_folder = MenuItem::new("Open screenshots folder", true, None);
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart_enabled, None);
    let quit = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &capture,
        &open_folder,
        &autostart_item,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .context("building tray menu")?;

    let tray = TrayIconBuilder::new()
        .with_icon(normal_icon.clone())
        .with_tooltip("Glimt")
        .with_menu(Box::new(menu))
        .build()
        .context("creating tray icon")?;

    Ok(Tray {
        icon: tray,
        normal_icon,
        rec_icon,
        capture_id: capture.id().clone(),
        open_folder_id: open_folder.id().clone(),
        autostart_item,
        quit_id: quit.id().clone(),
    })
}
