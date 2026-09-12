#![cfg(feature = "mp4")]

use std::sync::Arc;

use decode::{
    DecodeError, Decoder, Frame, Mp4Decoder, PixelFormat, Source, SourceId, SourceStreamId,
};
use time::RationalTime;

/// 64x64, 30 fps, 1 s, Constrained Baseline, keyframes at frames 0 and 15.
/// Frames 0..15 are solid red, 15..30 solid blue. Generated with ffmpeg 8.0:
/// `color=c=red` + `color=c=blue` concat, libx264 `-g 15 -bf 0`, yuv420p.
const FIXTURE: &[u8] = include_bytes!("fixtures/red_blue_64x64_30fps.mp4");

/// Centre pixel as ffmpeg decodes it (`-f rawvideo -pix_fmt rgba`); YUV round-trip, so not pure.
const RED: [u8; 4] = [253, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 254, 255];
/// Per-channel slack between two conforming H.264 decoders' YUV→RGB conversions.
const TOLERANCE: u8 = 4;

fn t(n: i64, d: u64) -> RationalTime {
    RationalTime::new(n, d).unwrap()
}

fn open() -> (Mp4Decoder, SourceId, SourceStreamId) {
    let mut decoder = Mp4Decoder::new();
    let source = decoder
        .open(&Source::Bytes(Arc::from(FIXTURE)))
        .expect("fixture opens");
    let streams = decoder.streams(source).unwrap();
    assert_eq!(streams.len(), 1, "one video stream");
    (decoder, source, streams[0].id)
}

fn centre(frame: &Frame) -> [u8; 4] {
    assert_eq!(frame.format, PixelFormat::Rgba8);
    let idx = ((frame.height / 2) * frame.width + frame.width / 2) as usize * 4;
    frame.pixels[idx..idx + 4].try_into().unwrap()
}

fn assert_colour(actual: [u8; 4], expected: [u8; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            a.abs_diff(e) <= TOLERANCE,
            "pixel {actual:?} not within {TOLERANCE} of {expected:?}"
        );
    }
}

#[test]
fn reports_stream_metadata() {
    let (decoder, source, stream) = open();
    let streams = decoder.streams(source).unwrap();
    let s = &streams[0];
    assert_eq!(s.id, stream);
    assert_eq!((s.width, s.height), (64, 64));
    assert_eq!(s.frame_rate.period(), t(1, 30));
    assert_eq!(s.duration, t(1, 1));
}

#[test]
fn first_frame_is_red_at_time_zero() {
    let (mut decoder, source, stream) = open();
    let frame = decoder.read_frame(source, stream, t(0, 1)).unwrap();
    assert_eq!(frame.time, t(0, 1));
    assert_eq!((frame.width, frame.height), (64, 64));
    assert_eq!(frame.pixels.len(), 64 * 64 * 4);
    assert_colour(centre(&frame), RED);
}

#[test]
fn sequential_reads_cross_the_colour_change_at_the_second_keyframe() {
    let (mut decoder, source, stream) = open();
    let f14 = decoder.read_frame(source, stream, t(14, 30)).unwrap();
    assert_eq!(f14.time, t(14, 30));
    assert_colour(centre(&f14), RED);

    let f15 = decoder.read_frame(source, stream, t(15, 30)).unwrap();
    assert_eq!(f15.time, t(15, 30));
    assert_colour(centre(&f15), BLUE);

    let f29 = decoder.read_frame(source, stream, t(29, 30)).unwrap();
    assert_eq!(f29.time, t(29, 30));
    assert_colour(centre(&f29), BLUE);
}

#[test]
fn backward_seek_reseeks_to_the_preceding_keyframe() {
    let (mut decoder, source, stream) = open();
    let late = decoder.read_frame(source, stream, t(29, 30)).unwrap();
    assert_colour(centre(&late), BLUE);

    // Frame 3 is a P-frame after keyframe 0: the driver must restart from 0, not continue.
    let early = decoder.read_frame(source, stream, t(3, 30)).unwrap();
    assert_eq!(early.time, t(3, 30));
    assert_colour(centre(&early), RED);
}

#[test]
fn times_between_frames_floor_to_the_current_frame() {
    let (mut decoder, source, stream) = open();
    // 1/45 s lies between frame 0 (0 s) and frame 1 (1/30 s).
    let frame = decoder.read_frame(source, stream, t(1, 45)).unwrap();
    assert_eq!(frame.time, t(0, 1));
    // 0.51 s lies between frame 15 (0.5 s) and frame 16.
    let frame = decoder.read_frame(source, stream, t(51, 100)).unwrap();
    assert_eq!(frame.time, t(15, 30));
    assert_colour(centre(&frame), BLUE);
}

#[test]
fn at_or_past_duration_is_end_of_stream() {
    let (mut decoder, source, stream) = open();
    assert_eq!(
        decoder.read_frame(source, stream, t(1, 1)).err(),
        Some(DecodeError::EndOfStream)
    );
    assert_eq!(
        decoder.read_frame(source, stream, t(7, 2)).err(),
        Some(DecodeError::EndOfStream)
    );
}

#[test]
fn unknown_stream_and_closed_source_error() {
    let (mut decoder, source, _stream) = open();
    assert_eq!(
        decoder
            .read_frame(source, SourceStreamId::new(99), t(0, 1))
            .err(),
        Some(DecodeError::StreamNotFound)
    );
    decoder.close(source).unwrap();
    assert_eq!(
        decoder.streams(source).err(),
        Some(DecodeError::SourceNotFound)
    );
    assert_eq!(
        decoder.close(source).err(),
        Some(DecodeError::SourceNotFound)
    );
}

#[test]
fn bytes_that_are_not_mp4_are_unsupported() {
    let mut decoder = Mp4Decoder::new();
    let err = decoder
        .open(&Source::Bytes(Arc::from(
            &b"definitely not an mp4 file"[..],
        )))
        .err();
    assert_eq!(err, Some(DecodeError::UnsupportedFormat));
}

#[test]
fn opens_from_a_path() {
    let path = std::env::temp_dir().join(format!(
        "opencut-decode-{}-{}.mp4",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, FIXTURE).unwrap();

    let mut decoder = Mp4Decoder::new();
    let source = decoder.open(&Source::Path(path.clone())).unwrap();
    let stream = decoder.streams(source).unwrap()[0].id;
    let frame = decoder.read_frame(source, stream, t(20, 30)).unwrap();
    assert_colour(centre(&frame), BLUE);

    let _ = std::fs::remove_file(path);
}

#[test]
fn missing_path_reports_the_path() {
    let mut decoder = Mp4Decoder::new();
    let err = decoder
        .open(&Source::Path("/definitely/missing/opencut.mp4".into()))
        .err();
    match err {
        Some(DecodeError::DecodingFailed(message)) => {
            assert!(message.contains("opencut.mp4"), "{message}")
        }
        other => panic!("expected DecodingFailed, got {other:?}"),
    }
}
