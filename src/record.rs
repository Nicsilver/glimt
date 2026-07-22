use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::VideoFormat;

pub trait VideoSink {
    /// bgra is bottom-up w*h*4; t is elapsed since recording start.
    fn write(&mut self, bgra: &[u8], t: Duration) -> Result<()>;
    /// Finish the file and return its (temp) path.
    fn finish(self: Box<Self>) -> Result<PathBuf>;
}

pub enum RecorderMsg {
    Done(PathBuf),
    Discarded,
    Failed(String),
}

pub struct RecorderHandle {
    stop: Arc<AtomicU8>, // 0 run, 1 stop+save, 2 discard
    pub rx: mpsc::Receiver<RecorderMsg>,
}

impl RecorderHandle {
    pub fn stop(&self) {
        self.stop.store(1, Ordering::Relaxed);
    }
    pub fn discard(&self) {
        self.stop.store(2, Ordering::Relaxed);
    }
}

pub fn start(region: (i32, i32, u32, u32), format: VideoFormat) -> RecorderHandle {
    let stop = Arc::new(AtomicU8::new(0));
    let (tx, rx) = mpsc::channel();
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run(region, format, &stop));
        });
    }
    RecorderHandle { stop, rx }
}

fn run(region: (i32, i32, u32, u32), format: VideoFormat, stop: &AtomicU8) -> RecorderMsg {
    let ext = match format {
        VideoFormat::Mp4 => "mp4",
        VideoFormat::Gif => "gif",
    };
    let tmp = match crate::config::save_dir() {
        Ok(dir) => dir.join(format!(".glimt_rec_tmp.{ext}")),
        Err(e) => return RecorderMsg::Failed(format!("{e:#}")),
    };
    match record_loop(region, format, ext, &tmp, stop) {
        Ok(Some(path)) => RecorderMsg::Done(path),
        Ok(None) => {
            let _ = std::fs::remove_file(&tmp);
            RecorderMsg::Discarded
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            RecorderMsg::Failed(format!("{e:#}"))
        }
    }
}

fn record_loop(
    region: (i32, i32, u32, u32),
    format: VideoFormat,
    ext: &str,
    tmp: &Path,
    stop: &AtomicU8,
) -> Result<Option<PathBuf>> {
    let (x, y, w, h) = region;
    // The gif frame_dur must stay 70 ms: GifSink stamps every frame with delay=7
    // (10 ms units), and pacing and metadata have to agree.
    let (frame_dur, mut sink): (Duration, Box<dyn VideoSink>) = match format {
        VideoFormat::Mp4 => (
            Duration::from_secs(1) / 30,
            Box::new(crate::encode_mp4::Mp4Sink::new(tmp, w, h)?),
        ),
        VideoFormat::Gif => (
            Duration::from_millis(70),
            Box::new(crate::encode_gif::GifSink::new(tmp, w, h)?),
        ),
    };

    let start = Instant::now();
    let mut buf: Vec<u8> = Vec::new();
    // CFR slot pacing: frame n belongs at start + n*frame_dur. If capture falls
    // behind, slots are skipped and n jumps ahead, so timestamps stay real-time
    // and players hold the last frame. Never accumulate drift with naive sleeps.
    let mut n: u64 = 0;
    loop {
        match stop.load(Ordering::Relaxed) {
            1 => {
                let path = sink.finish()?;
                let dest = crate::config::save_dir()?.join(crate::config::filename_now(ext));
                std::fs::rename(&path, &dest)?;
                return Ok(Some(dest));
            }
            2 => {
                // Finish (not just drop) so the file handles close and the caller
                // can delete the temp file.
                let _ = sink.finish();
                return Ok(None);
            }
            _ => {}
        }
        let elapsed = start.elapsed();
        let cur_slot = (elapsed.as_nanos() / frame_dur.as_nanos()) as u64;
        if cur_slot < n {
            // Next slot is in the future; nap in short chunks so stop flags are
            // seen quickly.
            let due = frame_dur * n as u32;
            std::thread::sleep((due - elapsed).min(Duration::from_millis(10)));
            continue;
        }
        n = n.max(cur_slot);
        crate::capture::capture_region_bgra(x, y, w, h, &mut buf)?;
        sink.write(&buf, frame_dur * n as u32)?;
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VideoFormat;

    /// End-to-end recorder smoke test, UI-free. Ignored by default because it
    /// captures the live screen and writes into Pictures\Glimt; run with
    /// `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "captures the live screen"]
    fn smoke_record_mp4_and_gif() {
        for format in [VideoFormat::Mp4, VideoFormat::Gif] {
            let handle = start((100, 100, 320, 240), format);
            std::thread::sleep(Duration::from_secs(3));
            handle.stop();
            match handle.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(RecorderMsg::Done(path)) => {
                    let len = std::fs::metadata(&path).unwrap().len();
                    println!("SMOKE_OK {} {}", path.display(), len);
                    assert!(len > 10_000, "suspiciously small file: {len} bytes");
                }
                Ok(RecorderMsg::Discarded) => panic!("unexpected discard"),
                Ok(RecorderMsg::Failed(e)) => panic!("recording failed: {e}"),
                Err(e) => panic!("no recorder message: {e}"),
            }
        }
    }
}
