use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};

use crate::record::VideoSink;

/// MP4 via WinRT MediaTranscoder (hardware-accelerated, codecs ship with Windows):
/// no ffmpeg, no bundled codecs, keeps the exe portable.
pub struct Mp4Sink {
    encoder: VideoEncoder,
    path: PathBuf,
}

impl Mp4Sink {
    pub fn new(path: &Path, w: u32, h: u32) -> Result<Self> {
        let encoder = VideoEncoder::new(
            VideoSettingsBuilder::new(w, h)
                .sub_type(VideoSettingsSubType::H264) // default is HEVC: unplayable in Discord/browsers
                .frame_rate(30) // default is 60
                .bitrate((w * h * 3).max(2_000_000)), // ~6 Mbit at 1080p; default 15 Mbit is overkill
            AudioSettingsBuilder::default().disabled(true),
            ContainerSettingsBuilder::default(), // mp4
            path,
        )
        .context("creating MP4 encoder")?;
        Ok(Mp4Sink {
            encoder,
            path: path.to_path_buf(),
        })
    }
}

impl VideoSink for Mp4Sink {
    fn write(&mut self, bgra: &[u8], t: Duration) -> Result<()> {
        // Timestamps are 100 ns ticks; the buffer is bottom-up BGRA as captured.
        self.encoder
            .send_frame_buffer(bgra, t.as_nanos() as i64 / 100)
            .context("sending frame to MP4 encoder")
    }

    fn finish(self: Box<Self>) -> Result<PathBuf> {
        self.encoder.finish().context("finishing MP4")?;
        Ok(self.path)
    }
}
