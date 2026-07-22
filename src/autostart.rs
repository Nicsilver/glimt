use anyhow::{Context, Result};
use auto_launch::{AutoLaunch, AutoLaunchBuilder};

pub fn manager() -> Result<AutoLaunch> {
    let exe = std::env::current_exe()?;
    AutoLaunchBuilder::new()
        .set_app_name("Glimt")
        .set_app_path(exe.to_string_lossy().as_ref())
        .build()
        .context("building autostart manager")
}

/// Re-enable on every startup so the Run key tracks the exe if it moved.
pub fn sync(enabled: bool) {
    if let Ok(m) = manager() {
        let _ = if enabled { m.enable() } else { m.disable() };
    }
}
