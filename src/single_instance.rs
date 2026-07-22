use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Holds the named mutex for the process lifetime; the OS releases it on exit.
pub struct SingleInstance {
    _handle: HANDLE,
}

pub fn acquire() -> Option<SingleInstance> {
    unsafe {
        let handle = CreateMutexW(None, false, w!("Glimt.SingleInstance")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(SingleInstance { _handle: handle })
    }
}
