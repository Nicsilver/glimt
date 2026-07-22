use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use gif::{Encoder, Frame, Repeat};

use crate::record::VideoSink;

pub struct GifSink {
    encoder: Encoder<BufWriter<File>>,
    path: PathBuf,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

impl GifSink {
    pub fn new(path: &Path, w: u32, h: u32) -> Result<Self> {
        let file = File::create(path).context("creating gif file")?;
        let mut encoder =
            Encoder::new(BufWriter::new(file), w as u16, h as u16, &[]).context("gif encoder")?;
        encoder.set_repeat(Repeat::Infinite).context("gif repeat")?;
        Ok(GifSink {
            encoder,
            path: path.to_path_buf(),
            w,
            h,
            rgba: Vec::new(),
        })
    }
}

impl VideoSink for GifSink {
    fn write(&mut self, bgra: &[u8], _t: Duration) -> Result<()> {
        // One pass flips the bottom-up rows top-down and swaps B/R channels.
        let (w, h) = (self.w as usize, self.h as usize);
        self.rgba.resize(w * h * 4, 0);
        for (dst_row, src_row) in self
            .rgba
            .chunks_exact_mut(w * 4)
            .zip(bgra.chunks_exact(w * 4).rev())
        {
            for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = 255;
            }
        }
        // NeuQuant palette per frame (speed 10) is the known CPU cost; the
        // recorder's slot-skipping pacing absorbs it.
        let mut frame = Frame::from_rgba_speed(self.w as u16, self.h as u16, &mut self.rgba, 10);
        frame.delay = 7; // 10 ms units; 70 ms matches the recorder's gif frame_dur
        self.encoder.write_frame(&frame).context("gif frame")
    }

    fn finish(self: Box<Self>) -> Result<PathBuf> {
        drop(self.encoder); // flushes the trailer
        Ok(self.path)
    }
}
