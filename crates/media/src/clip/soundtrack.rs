use std::path::Path;

use glint_core::clip::pcm;
use windows::Win32::Media::MediaFoundation::{IMFSourceReader, MF_SOURCE_READER_FIRST_AUDIO_STREAM};

use super::media::{self, ALL, Got};
use crate::record::Ctx;

const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

/// A standalone audio file, decoded in timestamped stereo chunks at its native rate.
/// Open and use it on a thread with Media Foundation initialized.
pub struct Soundtrack {
    reader: IMFSourceReader,
    duration: i64,
    rate: u32,
    channels: usize,
    scratch: Vec<f32>,
}

impl Soundtrack {
    pub fn open(path: &Path) -> Result<Self, String> {
        let reader = media::open(path, None)?;
        unsafe {
            reader.SetStreamSelection(ALL, false).ctx("disable streams")?;
            reader.SetStreamSelection(AUDIO, true).ctx("select the first audio stream")?;
        }
        let (rate, channels) = media::float_native(&reader, AUDIO)?;
        let duration = media::length(&reader)?;
        if duration <= 0 {
            return Err("the audio file has no duration".into());
        }

        Ok(Self { reader, duration, rate, channels, scratch: Vec::new() })
    }

    /// Source duration in 100 ns units.
    pub fn duration(&self) -> i64 {
        self.duration
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Seek in source time. The decoder may return samples from before `at`.
    pub fn seek(&mut self, at: i64) -> Result<(), String> {
        media::seek(&self.reader, at)
    }

    /// Replace `out` with one interleaved stereo chunk and return its source timestamp.
    /// At end of stream, clear `out` and return `None`.
    pub fn read_stereo(&mut self, out: &mut Vec<f32>) -> Result<Option<i64>, String> {
        match media::read(&self.reader, AUDIO)? {
            Got::Sample { ts, sample, .. } => {
                media::floats(&sample, &mut self.scratch)?;
                pcm::to_stereo(&self.scratch, self.channels, out);
                Ok(Some(ts))
            }
            Got::End(_) => {
                out.clear();
                Ok(None)
            }
        }
    }
}
