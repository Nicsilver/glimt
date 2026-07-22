use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW,
    HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, ReleaseDC, SRCCOPY, SelectObject,
};

pub struct MonitorShot {
    /// x, y, w, h in physical virtual-screen px.
    pub rect: (i32, i32, i32, i32),
    pub image: image::RgbaImage,
}

pub trait ScreenCapturer {
    fn capture_all(&self) -> Result<Vec<MonitorShot>>;
}

pub struct GdiCapturer;

impl ScreenCapturer for GdiCapturer {
    fn capture_all(&self) -> Result<Vec<MonitorShot>> {
        let monitors = enumerate_monitors();
        if monitors.is_empty() {
            bail!("no monitors found");
        }
        // Capture monitors concurrently; each thread owns its own DCs, which GDI allows.
        std::thread::scope(|s| {
            let handles: Vec<_> = monitors
                .into_iter()
                .map(|r| s.spawn(move || capture_monitor(r)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("capture thread panicked"))
                .collect()
        })
    }
}

/// Current monitor rects (x, y, w, h in physical virtual-screen px), without capturing.
pub fn monitor_rects() -> Vec<(i32, i32, i32, i32)> {
    enumerate_monitors()
        .iter()
        .map(|r| (r.left, r.top, r.right - r.left, r.bottom - r.top))
        .collect()
}

fn enumerate_monitors() -> Vec<RECT> {
    unsafe extern "system" fn cb(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        let rects = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            rects.push(info.monitorInfo.rcMonitor);
        }
        TRUE
    }

    let mut rects: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut rects as *mut _ as isize));
    }
    rects
}

fn capture_monitor(rect: RECT) -> Result<MonitorShot> {
    let (x, y) = (rect.left, rect.top);
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            bail!("GetDC failed");
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
        let old = SelectObject(mem_dc, bitmap.into());

        // No CAPTUREBLT: with DWM compositing the screen DC already includes layered
        // windows, and the flag forces a slow sync (and cursor blink) per monitor.
        let blit = BitBlt(mem_dc, 0, 0, w, h, Some(screen_dc), x, y, SRCCOPY);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down rows
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w as usize) * (h as usize) * 4];
        let lines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            h as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        blit.context("BitBlt failed")?;
        if lines == 0 {
            bail!("GetDIBits failed");
        }

        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
            px[3] = 255;
        }
        let image = image::RgbaImage::from_raw(w as u32, h as u32, bgra)
            .context("bitmap buffer size mismatch")?;
        Ok(MonitorShot {
            rect: (x, y, w, h),
            image,
        })
    }
}
