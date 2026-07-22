use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VideoFormat {
    Mp4,
    Gif,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub autostart: bool,
    pub prtsc_warning_shown: bool,
    pub video_format: VideoFormat,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            autostart: true,
            prtsc_warning_shown: false,
            video_format: VideoFormat::Mp4,
        }
    }
}

fn settings_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("no config dir")?.join("Glimt");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        settings_path()
            .and_then(|p| Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(path) = settings_path() {
            let _ = std::fs::write(path, serde_json::to_string_pretty(self).unwrap());
        }
    }
}

pub fn save_dir() -> Result<PathBuf> {
    let dir = dirs::picture_dir()
        .context("no Pictures dir")?
        .join("Glimt");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn filename_now(ext: &str) -> String {
    format!(
        "glimt_{}.{ext}",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    )
}
