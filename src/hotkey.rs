use anyhow::{Context, Result};
use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
use windows::core::w;

pub struct Hotkey {
    // Kept alive for the process lifetime; dropping unregisters the hotkeys.
    pub _manager: GlobalHotKeyManager,
    pub prtsc_id: u32,
    pub shift_prtsc_id: u32,
}

pub fn register_prtsc() -> Result<Hotkey> {
    let manager = GlobalHotKeyManager::new().context("creating hotkey manager")?;
    let prtsc = HotKey::new(None, Code::PrintScreen);
    let shift_prtsc = HotKey::new(Some(Modifiers::SHIFT), Code::PrintScreen);
    let (prtsc_id, shift_prtsc_id) = (prtsc.id(), shift_prtsc.id());
    if manager.register(prtsc).is_err() {
        // Not fatal: capture still works from the tray. Warn so the user knows why
        // PrtSc does nothing (another screenshot app owns the key).
        unsafe {
            MessageBoxW(
                None,
                w!(
                    "Glimt could not grab the PrtSc key because another screenshot app (e.g. Lightshot, ShareX, OneDrive) owns it. Close that app and restart Glimt, or capture from the tray icon instead."
                ),
                w!("Glimt"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
    // Shift+PrtSc failing alone is unlikely; stay silent (video mode still
    // reachable via the overlay's Photo/Video switch).
    let _ = manager.register(shift_prtsc);
    Ok(Hotkey {
        _manager: manager,
        prtsc_id,
        shift_prtsc_id,
    })
}

/// Windows' own "PrtSc opens Snipping Tool" setting swallows the key before we see it.
/// Warn once; the user has to flip the toggle themselves (we don't write the key).
pub fn warn_if_snipping_owns_prtsc(settings: &mut crate::config::Settings) {
    if settings.prtsc_warning_shown {
        return;
    }
    if snipping_enabled() != Some(1) {
        return;
    }
    unsafe {
        MessageBoxW(
            None,
            w!(
                "Windows is set to open Snipping Tool with the Print Screen key, which blocks Glimt.\n\nTurn off \"Use the Print screen key to open screen capture\" in Settings > Accessibility > Keyboard, then press PrtSc to capture with Glimt."
            ),
            w!("Glimt"),
            MB_OK | MB_ICONWARNING,
        );
    }
    settings.prtsc_warning_shown = true;
    settings.save();
}

fn snipping_enabled() -> Option<u32> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value)
}
