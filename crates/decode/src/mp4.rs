//! MP4 (ISO BMFF) container with an H.264/AVC video track, decoded in software by OpenH264.
//!
//! Scope of this driver:
//! - one video track per source (the first `avc1` track); audio and other tracks are ignored
//! - decode order must equal presentation order (no B-frames). Streams that reorder are
//!   rejected at [`Decoder::open`] with [`DecodeError::UnsupportedFormat`] instead of
//!   returning frames at the wrong times
//! - random access seeks to the nearest preceding sync sample and decodes forward; sequential
//!   reads continue from the last decoded sample without reseeking

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use openh264::decoder::Decoder as H264;
use openh264::formats::YUVSource;
use re_mp4::{Mp4, Sample, StsdBoxContent, TrackKind};
use time::{FrameRate, RationalTime};

use crate::{
    DecodeError, Decoder, Frame, PixelFormat, Source, SourceId, SourceStream, SourceStreamId,
};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

struct VideoTrack {
    id: u32,
    timescale: u64,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    duration: RationalTime,
    /// End of the last sample in time units; times at or past this are [`DecodeError::EndOfStream`].
    end_units: i64,
    /// In decode order, which this driver requires to also be presentation order.
    samples: Vec<Sample>,
    /// SPS and PPS from `avcC`, already in Annex B form. Prepended to every sync sample.
    parameter_sets: Vec<u8>,
    /// Bytes per NAL length prefix inside samples (1, 2, or 4).
    length_size: usize,
}

struct OpenSource {
    bytes: Arc<[u8]>,
    track: VideoTrack,
    h264: H264,
    /// Index of the sample the decoder will consume next, when `primed`.
    next: usize,
    primed: bool,
}

/// [`Decoder`] for MP4 files carrying H.264 video.
pub struct Mp4Decoder {
    next_id: u64,
    sources: HashMap<u64, OpenSource>,
}

impl Default for Mp4Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Mp4Decoder {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            sources: HashMap::new(),
        }
    }

    fn lookup(&self, source: SourceId) -> Result<&OpenSource, DecodeError> {
        self.sources
            .get(&source.raw())
            .ok_or(DecodeError::SourceNotFound)
    }

    fn lookup_mut(&mut self, source: SourceId) -> Result<&mut OpenSource, DecodeError> {
        self.sources
            .get_mut(&source.raw())
            .ok_or(DecodeError::SourceNotFound)
    }
}

impl Decoder for Mp4Decoder {
    fn open(&mut self, source: &Source) -> Result<SourceId, DecodeError> {
        let bytes = load(source)?;
        let mp4 = Mp4::read_bytes(&bytes).map_err(|_| DecodeError::UnsupportedFormat)?;
        let track = select_video_track(&mp4)?;
        let h264 = H264::new().map_err(failed)?;

        let id = SourceId::new(self.next_id);
        self.next_id += 1;
        self.sources.insert(
            id.raw(),
            OpenSource {
                bytes,
                track,
                h264,
                next: 0,
                primed: false,
            },
        );
        Ok(id)
    }

    fn streams(&self, source: SourceId) -> Result<Vec<SourceStream>, DecodeError> {
        let track = &self.lookup(source)?.track;
        Ok(vec![SourceStream {
            id: SourceStreamId::new(track.id as u64),
            frame_rate: track.frame_rate,
            width: track.width,
            height: track.height,
            duration: track.duration,
        }])
    }

    fn read_frame(
        &mut self,
        source: SourceId,
        stream: SourceStreamId,
        time: RationalTime,
    ) -> Result<Frame, DecodeError> {
        let open = self.lookup_mut(source)?;
        let track = &open.track;
        if stream.raw() != track.id as u64 {
            return Err(DecodeError::StreamNotFound);
        }

        let units = to_units(time, track.timescale)?;
        if units >= track.end_units {
            return Err(DecodeError::EndOfStream);
        }
        let target = track
            .samples
            .partition_point(|s| s.composition_timestamp <= units)
            .checked_sub(1)
            .ok_or(DecodeError::EndOfStream)?;

        // Continue from where the decoder is, or reseek to the closest sync sample at or before
        // the target. A fresh decoder on reseek guarantees no state from the abandoned position
        // leaks into the new run.
        let start = if open.primed && open.next <= target {
            open.next
        } else {
            let sync = track.samples[..=target]
                .iter()
                .rposition(|s| s.is_sync)
                .ok_or_else(|| failed("no sync sample precedes the requested time"))?;
            open.h264 = H264::new().map_err(failed)?;
            open.primed = false;
            sync
        };

        let mut packet = Vec::new();
        let mut picture = None;
        for index in start..=target {
            let sample = &track.samples[index];
            packet.clear();
            if sample.is_sync {
                packet.extend_from_slice(&track.parameter_sets);
            }
            let range = sample.byte_range();
            let data = open
                .bytes
                .get(range)
                .ok_or_else(|| failed("sample range exceeds file"))?;
            to_annex_b(data, track.length_size, &mut packet)?;

            let decoded = open.h264.decode(&packet).map_err(failed)?;
            if index == target {
                picture = match decoded {
                    Some(yuv) => Some(to_rgba(&yuv)),
                    // Nothing came out for the target; whatever is buffered belongs to it.
                    None => open
                        .h264
                        .flush_remaining()
                        .map_err(failed)?
                        .last()
                        .map(to_rgba),
                };
            }
        }
        open.next = target + 1;
        open.primed = true;

        let (width, height, pixels) =
            picture.ok_or_else(|| failed("decoder produced no picture for the requested time"))?;
        if width != track.width || height != track.height {
            return Err(failed(format!(
                "decoded {width}x{height} but the track declares {}x{}",
                track.width, track.height
            )));
        }

        let sample = &track.samples[target];
        Ok(Frame {
            source,
            stream,
            time: RationalTime::new(sample.composition_timestamp, track.timescale)
                .map_err(failed)?,
            width,
            height,
            format: PixelFormat::Rgba8,
            pixels: pixels.into_boxed_slice(),
        })
    }

    fn close(&mut self, source: SourceId) -> Result<(), DecodeError> {
        self.sources
            .remove(&source.raw())
            .map(|_| ())
            .ok_or(DecodeError::SourceNotFound)
    }
}

fn load(source: &Source) -> Result<Arc<[u8]>, DecodeError> {
    match source {
        Source::Bytes(bytes) => Ok(bytes.clone()),
        #[cfg(not(target_family = "wasm"))]
        Source::Path(path) => std::fs::read(path)
            .map(Arc::from)
            .map_err(|error| failed(format!("{}: {error}", path.display()))),
        #[cfg(target_family = "wasm")]
        Source::Path(_) => Err(DecodeError::UnsupportedFormat),
    }
}

fn select_video_track(mp4: &Mp4) -> Result<VideoTrack, DecodeError> {
    let track = mp4
        .tracks()
        .values()
        .find(|track| track.kind == Some(TrackKind::Video))
        .ok_or(DecodeError::UnsupportedFormat)?;

    let avc1 = match &track.trak(mp4).mdia.minf.stbl.stsd.contents {
        StsdBoxContent::Avc1(avc1) => avc1,
        _ => return Err(DecodeError::UnsupportedFormat),
    };
    let avcc = &avc1.avcc;

    let length_size = avcc.length_size_minus_one as usize + 1;
    if !matches!(length_size, 1 | 2 | 4) {
        return Err(DecodeError::UnsupportedFormat);
    }
    let mut parameter_sets = Vec::new();
    for nal in avcc
        .sequence_parameter_sets
        .iter()
        .chain(&avcc.picture_parameter_sets)
    {
        parameter_sets.extend_from_slice(&START_CODE);
        parameter_sets.extend_from_slice(&nal.bytes);
    }
    if parameter_sets.is_empty() {
        return Err(DecodeError::UnsupportedFormat);
    }

    let samples = track.samples.clone();
    if samples.is_empty() || track.timescale == 0 {
        return Err(DecodeError::UnsupportedFormat);
    }
    // Presentation order must match decode order; B-frames are not handled yet.
    if samples
        .windows(2)
        .any(|pair| pair[1].composition_timestamp <= pair[0].composition_timestamp)
    {
        return Err(DecodeError::UnsupportedFormat);
    }

    let mut duration_counts: HashMap<u64, usize> = HashMap::new();
    for sample in &samples {
        *duration_counts.entry(sample.duration).or_default() += 1;
    }
    let typical_duration = duration_counts
        .into_iter()
        .filter(|(duration, _)| *duration > 0)
        .max_by_key(|(duration, count)| (*count, std::cmp::Reverse(*duration)))
        .map(|(duration, _)| duration)
        .ok_or(DecodeError::UnsupportedFormat)?;
    let g = gcd(track.timescale, typical_duration);
    let frame_rate = FrameRate::new(track.timescale / g, typical_duration / g).map_err(failed)?;

    let end_units = samples
        .iter()
        .map(|s| s.composition_timestamp + s.duration as i64)
        .max()
        .unwrap_or(0);
    let duration = RationalTime::new(end_units, track.timescale).map_err(failed)?;

    let (width, height) = if track.width > 0 && track.height > 0 {
        (track.width, track.height)
    } else {
        (avc1.width, avc1.height)
    };
    if width == 0 || height == 0 {
        return Err(DecodeError::UnsupportedFormat);
    }

    Ok(VideoTrack {
        id: track.track_id,
        timescale: track.timescale,
        width: width as u32,
        height: height as u32,
        frame_rate,
        duration,
        end_units,
        samples,
        parameter_sets,
        length_size,
    })
}

/// Rewrites a length-prefixed MP4 sample as Annex B NAL units.
fn to_annex_b(sample: &[u8], length_size: usize, out: &mut Vec<u8>) -> Result<(), DecodeError> {
    let mut cursor = 0;
    while cursor < sample.len() {
        let prefix = sample
            .get(cursor..cursor + length_size)
            .ok_or_else(|| failed("truncated NAL length prefix"))?;
        let length = prefix
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | *byte as usize);
        cursor += length_size;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= sample.len())
            .ok_or_else(|| failed("NAL length exceeds sample"))?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&sample[cursor..end]);
        cursor = end;
    }
    Ok(())
}

fn to_rgba(yuv: &openh264::decoder::DecodedYUV<'_>) -> (u32, u32, Vec<u8>) {
    let (width, height) = yuv.dimensions();
    let mut rgba = vec![0u8; width * height * 4];
    yuv.write_rgba8(&mut rgba);
    (width as u32, height as u32, rgba)
}

/// Floor of `time` in track time units.
fn to_units(time: RationalTime, timescale: u64) -> Result<i64, DecodeError> {
    if time.numer() < 0 {
        return Err(failed("negative time"));
    }
    let scaled = (time.numer() as i128)
        .checked_mul(timescale as i128)
        .ok_or_else(|| failed("time overflow"))?;
    i64::try_from(scaled / time.denom() as i128).map_err(|_| failed("time overflow"))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        a %= b;
        std::mem::swap(&mut a, &mut b);
    }
    a
}

fn failed(error: impl Display) -> DecodeError {
    DecodeError::DecodingFailed(error.to_string())
}
