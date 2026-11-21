//! Audio and video metadata for the media preview card.
//!
//! Windows' Shell Property Store would need a COM round trip per file; the
//! preview needs only a handful of header fields and must never block the UI
//! thread, so this module parses the container headers directly (zero
//! dependencies, unit-testable with synthetic files):
//!
//! - MP4/MOV/M4A (`moov` → `mvhd`, `trak`, `mdhd`, `hdlr`, `stsd`)
//! - Matroska/WebM (EBML `Info` and `Tracks`)
//! - AVI (RIFF `avih`, `strh`, `strf`)
//! - WAV (`fmt `, `data`), FLAC (`STREAMINFO`), MP3 (frame header + `Xing`)
//! - OGG/Opus (first page + last granule)
//!
//! Anything unknown or damaged yields a [`PreviewError`], which the caller
//! turns into an Unsupported card with a `View as Hex` button. Missing fields
//! are simply absent from the rows; a partial table is better than no card.

use super::{MediaKind, MediaRow, PreviewError};
use crate::meta::foldersize::format_bytes;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes read from the front of a container for the header parsers.
pub const MEDIA_HEADER_BYTES: u64 = 4 << 20;

/// Largest `moov`/`mvex` body read into memory (MP4 metadata boxes are small).
const MAX_MOOV_BYTES: u64 = 16 << 20;

/// Rows the media card shows: kind plus ordered key/value pairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub rows: Vec<MediaRow>,
}

/// Fields collected by a parser, rendered in a stable order by [`Parts::finish`].
#[derive(Default)]
struct Parts {
    has_video: bool,
    has_audio: bool,
    duration: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    bitrate: Option<f64>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

impl Parts {
    fn finish(self, container: &str, file_size: u64) -> MediaInfo {
        let mut rows: Vec<MediaRow> = Vec::new();
        if let Some(d) = self.duration.filter(|d| d.is_finite() && *d > 0.0) {
            rows.push(("Duration".into(), format_duration(d)));
        }
        if let (Some(w), Some(h)) = (self.width, self.height)
            && w > 0
            && h > 0
        {
            rows.push(("Resolution".into(), format!("{w} × {h}")));
        }
        if let Some(f) = self.fps.filter(|f| f.is_finite() && *f > 0.0) {
            rows.push(("Frame rate".into(), format!("{f:.2} fps")));
        }
        if let Some(b) = self.bitrate.filter(|b| *b > 0.0) {
            rows.push(("Bitrate".into(), format_bitrate(b)));
        }
        if let Some(c) = self.video_codec {
            rows.push(("Video codec".into(), c));
        }
        if let Some(c) = self.audio_codec {
            rows.push(("Audio codec".into(), c));
        }
        if let Some(sr) = self.sample_rate.filter(|s| *s > 0) {
            rows.push(("Sample rate".into(), format_sample_rate(sr)));
        }
        if let Some(ch) = self.channels.filter(|c| *c > 0) {
            rows.push(("Channels".into(), format_channels(ch)));
        }
        rows.push(("Container".into(), container.to_string()));
        rows.push(("File size".into(), format_bytes(file_size)));
        let kind = if self.has_video {
            MediaKind::Video
        } else {
            MediaKind::Audio
        };
        MediaInfo { kind, rows }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Reads the metadata of `path`. Unknown or damaged containers are errors.
pub fn load(path: &Path) -> Result<MediaInfo, PreviewError> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PreviewError::NotFound(path.display().to_string()),
        std::io::ErrorKind::PermissionDenied => PreviewError::AccessDenied(path.display().to_string()),
        _ => PreviewError::Io(format!("{}: {e}", path.display())),
    })?;
    if !meta.is_file() {
        return Err(PreviewError::Unsupported("not a file".into()));
    }
    if meta.len() == 0 {
        return Err(PreviewError::Unsupported("empty media file".into()));
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let size = meta.len();
    let container = if ext.is_empty() { "Media".to_string() } else { ext.to_ascii_uppercase() };
    match ext.as_str() {
        "mp4" | "m4v" | "mov" | "m4a" => {
            let mut parts = Parts::default();
            mp4_info(path, size, &mut parts)?;
            if ext == "m4a" && !parts.has_video {
                parts.has_audio = true;
                parts.audio_codec.get_or_insert_with(|| "AAC".into());
            }
            Ok(parts.finish(&container, size))
        }
        "mkv" | "webm" => {
            let mut parts = Parts::default();
            let head = read_head(path, MEDIA_HEADER_BYTES)?;
            if !head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
                return Err(PreviewError::Decode("not a Matroska file".into()));
            }
            ebml_info(&head, 0, &mut parts);
            if parts.duration.is_none() && parts.sample_rate.is_none() && parts.width.is_none() {
                return Err(PreviewError::Decode("no Matroska metadata".into()));
            }
            Ok(parts.finish(&container, size))
        }
        "avi" => {
            let mut parts = Parts::default();
            let head = read_head(path, MEDIA_HEADER_BYTES)?;
            if !(head.starts_with(b"RIFF") && head.get(8..12) == Some(b"AVI ")) {
                return Err(PreviewError::Decode("not an AVI file".into()));
            }
            avi_scan(&head[12..], 0, &mut parts);
            if parts.width.is_none() && parts.duration.is_none() {
                return Err(PreviewError::Decode("no AVI metadata".into()));
            }
            Ok(parts.finish(&container, size))
        }
        "wav" => {
            let mut parts = Parts::default();
            let head = read_head(path, MEDIA_HEADER_BYTES)?;
            wav_info(&head, &mut parts)?;
            Ok(parts.finish(&container, size))
        }
        "flac" => {
            let mut parts = Parts::default();
            let head = read_head(path, MEDIA_HEADER_BYTES)?;
            flac_info(&head, size, &mut parts)?;
            Ok(parts.finish(&container, size))
        }
        "mp3" => {
            let mut parts = Parts::default();
            let head = read_head(path, MEDIA_HEADER_BYTES)?;
            mp3_info(&head, size, &mut parts)?;
            Ok(parts.finish(&container, size))
        }
        "ogg" | "oga" | "opus" => {
            let mut parts = Parts::default();
            ogg_info(path, size, &mut parts)?;
            Ok(parts.finish(&container, size))
        }
        // Containers we only label: the card still shows size and type.
        "aac" | "wma" | "wmv" | "flv" | "aiff" | "aif" => {
            let mut parts = Parts::default();
            let video = matches!(ext.as_str(), "wmv" | "flv");
            if video {
                parts.has_video = true;
                parts.video_codec = Some(ext.to_ascii_uppercase());
            } else {
                parts.has_audio = true;
                parts.audio_codec = Some(ext.to_ascii_uppercase());
            }
            Ok(parts.finish(&container, size))
        }
        other => Err(PreviewError::Unsupported(format!("{other} media"))),
    }
}

fn read_head(path: &Path, cap: u64) -> Result<Vec<u8>, PreviewError> {
    let file = File::open(path).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    Ok(buf)
}

fn read_at(file: &mut File, at: u64, len: usize) -> Result<Vec<u8>, PreviewError> {
    file.seek(SeekFrom::Start(at)).map_err(|e| PreviewError::Io(e.to_string()))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(|e| PreviewError::Io(e.to_string()))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// `M:SS`, or `H:MM:SS` past an hour.
pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `320 kbps`, `12.4 Mbps`, `900 bps`.
pub fn format_bitrate(bps: f64) -> String {
    if bps >= 1e6 {
        format!("{:.1} Mbps", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.0} kbps", bps / 1e3)
    } else {
        format!("{:.0} bps", bps)
    }
}

/// `44.1 kHz`, `48 kHz`, `8 kHz`, `7350 Hz`.
pub fn format_sample_rate(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else if hz.is_multiple_of(100) {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    } else {
        format!("{hz} Hz")
    }
}

/// `Mono`, `Stereo`, `5.1`, `7.1`, `4 channels`.
pub fn format_channels(n: u32) -> String {
    match n {
        1 => "Mono".into(),
        2 => "Stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        _ => format!("{n} channels"),
    }
}

// ---------------------------------------------------------------------------
// Byte helpers (all bounds-checked: a truncated file must not panic)
// ---------------------------------------------------------------------------

fn be16(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
fn be32(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
fn be64(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 8).map(|s| u64::from_be_bytes(s.try_into().expect("8 bytes")))
}
fn le16(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn le32(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn le64(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes")))
}

/// Splits a RIFF body into `(fourcc, payload)` chunks, honouring the pad byte.
fn riff_chunks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let mut id = [0u8; 4];
        id.copy_from_slice(&data[i..i + 4]);
        let size = le32(data, i + 4).unwrap_or(0) as usize;
        let start = i + 8;
        if start + size > data.len() {
            break;
        }
        out.push((id, &data[start..start + size]));
        i = start + size + (size & 1);
    }
    out
}

/// Splits an ISO BMFF body into `(type, payload)` boxes.
fn mp4_boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let mut ty = [0u8; 4];
        ty.copy_from_slice(&data[i + 4..i + 8]);
        let size32 = be32(data, i).unwrap_or(0) as usize;
        let (header, size) = if size32 == 1 {
            match be64(data, i + 8) {
                Some(s) => (16usize, s as usize),
                None => break,
            }
        } else if size32 == 0 {
            (8usize, data.len() - i)
        } else {
            (8usize, size32)
        };
        if size < header || i + size > data.len() {
            break;
        }
        out.push((ty, &data[i + header..i + size]));
        i += size;
    }
    out
}

// ---------------------------------------------------------------------------
// MP4 / MOV / M4A
// ---------------------------------------------------------------------------

fn mp4_info(path: &Path, size: u64, parts: &mut Parts) -> Result<(), PreviewError> {
    let mut file = File::open(path).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    let mut pos = 0u64;
    let mut moov: Option<Vec<u8>> = None;
    let mut boxes = 0;
    while pos + 8 <= size && boxes < 100_000 {
        boxes += 1;
        let len = ((size - pos).min(16)) as usize;
        let header = read_at(&mut file, pos, len)?;
        let size32 = be32(&header, 0).unwrap_or(0) as u64;
        let ty = &header[4..8];
        let (header_len, box_size) = if size32 == 1 {
            (16u64, be64(&header, 8).unwrap_or(0))
        } else if size32 == 0 {
            (8u64, size - pos)
        } else {
            (8u64, size32)
        };
        if box_size < header_len || pos + box_size > size {
            break;
        }
        if ty == b"moov" {
            let body = box_size - header_len;
            if body > MAX_MOOV_BYTES {
                return Err(PreviewError::Decode("moov box is too large".into()));
            }
            moov = Some(read_at(&mut file, pos + header_len, body as usize)?);
        }
        pos += box_size;
    }
    let moov = moov.ok_or_else(|| PreviewError::Decode("no moov box".into()))?;
    let mut duration = None;
    for (ty, body) in mp4_boxes(&moov) {
        match &ty {
            b"mvhd" => duration = later(duration, mp4_duration(body)),
            b"trak" => mp4_track(body, parts),
            _ => {}
        }
    }
    let total = later(duration, parts.duration);
    parts.duration = total;
    if let (Some(d), None) = (total, parts.bitrate)
        && d > 0.0
    {
        parts.bitrate = Some(size as f64 * 8.0 / d);
    }
    if !parts.has_video && !parts.has_audio {
        return Err(PreviewError::Decode("no audio or video track".into()));
    }
    Ok(())
}

/// The larger of two optional durations.
fn later(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (some, None) => some,
        (None, some) => some,
    }
}

/// Timescale and duration of `mvhd`/`mdhd`, in seconds.
fn mp4_duration(body: &[u8]) -> Option<f64> {
    let version = *body.first()?;
    let (scale, dur) = if version == 1 {
        (be32(body, 20)?, be64(body, 24)?)
    } else {
        (be32(body, 12)?, be32(body, 16)? as u64)
    };
    (scale > 0 && dur != u32::MAX as u64 && dur != u64::MAX).then(|| dur as f64 / scale as f64)
}

fn mp4_track(body: &[u8], parts: &mut Parts) {
    for (ty, sub) in mp4_boxes(body) {
        match &ty {
            b"tkhd" => {
                let version = *sub.first().unwrap_or(&0);
                let base = if version == 1 { 88 } else { 76 };
                let w = be32(sub, base).unwrap_or(0) >> 16;
                let h = be32(sub, base + 4).unwrap_or(0) >> 16;
                if w > 0 && h > 0 {
                    parts.width.get_or_insert(w);
                    parts.height.get_or_insert(h);
                }
            }
            b"mdia" => {
                let boxes = mp4_boxes(sub);
                let mut handler: Option<[u8; 4]> = None;
                let mut track_duration = None;
                for (ty, sub2) in &boxes {
                    match ty {
                        b"mdhd" => track_duration = later(track_duration, mp4_duration(sub2)),
                        b"hdlr" => handler = sub2.get(8..12).map(|h| [h[0], h[1], h[2], h[3]]),
                        _ => {}
                    }
                }
                let Some(handler) = handler else { continue };
                let video = &handler == b"vide";
                for (ty, sub2) in &boxes {
                    if ty == b"minf" {
                        mp4_minf(sub2, parts, video);
                    }
                }
                if video {
                    parts.has_video = true;
                    parts.duration = later(parts.duration, track_duration);
                } else if &handler == b"soun" {
                    parts.has_audio = true;
                }
            }
            _ => {}
        }
    }
}

fn mp4_minf(body: &[u8], parts: &mut Parts, video: bool) {
    for (ty, sub) in mp4_boxes(body) {
        if &ty != b"stbl" {
            continue;
        }
        for (ty, sub2) in mp4_boxes(sub) {
            if &ty == b"stsd" {
                mp4_stsd(sub2, parts, video);
            }
        }
    }
}

fn mp4_stsd(body: &[u8], parts: &mut Parts, video: bool) {
    // version/flags (4) + entry_count (4), then sample entries.
    let mut i = 8usize;
    while i + 8 <= body.len() {
        let size = be32(body, i).unwrap_or(0) as usize;
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(&body[i + 4..i + 8]);
        if size < 8 || i + size > body.len() {
            break;
        }
        if let Some(name) = codec_name(&fourcc) {
            if video {
                parts.video_codec.get_or_insert_with(|| name.into());
            } else {
                parts.audio_codec.get_or_insert_with(|| name.into());
            }
        }
        if video {
            parts.has_video = true;
            // Video sample entry: width at 32, height at 34 (16-bit).
            let w = be16(body, i + 32).unwrap_or(0) as u32;
            let h = be16(body, i + 34).unwrap_or(0) as u32;
            if w > 0 && h > 0 {
                parts.width.get_or_insert(w);
                parts.height.get_or_insert(h);
            }
        } else {
            parts.has_audio = true;
            // Audio sample entry: channelcount at 24, 16.16 sample rate at 32.
            let channels = be16(body, i + 24).unwrap_or(0) as u32;
            let rate = be32(body, i + 32).unwrap_or(0) >> 16;
            if channels > 0 {
                parts.channels.get_or_insert(channels);
            }
            if rate > 0 {
                parts.sample_rate.get_or_insert(rate);
            }
        }
        i += size;
    }
}

fn codec_name(fourcc: &[u8; 4]) -> Option<&'static str> {
    Some(match fourcc {
        b"avc1" | b"avc3" => "H.264",
        b"hvc1" | b"hev1" => "HEVC",
        b"vp08" => "VP8",
        b"vp09" => "VP9",
        b"av01" => "AV1",
        b"mp4v" => "MPEG-4",
        b"apch" | b"apcn" => "ProRes",
        b"mp4a" => "AAC",
        b"alac" => "ALAC",
        b"ac-3" => "AC-3",
        b"ec-3" => "E-AC-3",
        b"Opus" => "Opus",
        b"fLaC" => "FLAC",
        b"sowt" | b"twos" | b"lpcm" => "PCM",
        b".mp3" | b"mp3 " => "MP3",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Matroska / WebM (EBML)
// ---------------------------------------------------------------------------

fn ebml_id(b: &[u8], i: &mut usize) -> Option<u64> {
    let first = *b.get(*i)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || *i + len > b.len() {
        return None;
    }
    let mut v = 0u64;
    for k in 0..len {
        v = (v << 8) | b[*i + k] as u64;
    }
    *i += len;
    Some(v)
}

fn ebml_size(b: &[u8], i: &mut usize) -> Option<Option<u64>> {
    let first = *b.get(*i)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || *i + len > b.len() {
        return None;
    }
    let mask = (0xffu8 >> len) as u64;
    let mut all_ones = (first as u64 & mask) == mask;
    let mut v = first as u64 & mask;
    for k in 1..len {
        let byte = b[*i + k];
        all_ones &= byte == 0xff;
        v = (v << 8) | byte as u64;
    }
    *i += len;
    Some(if all_ones { None } else { Some(v) })
}

fn ebml_elements(data: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let Some(id) = ebml_id(data, &mut i) else { break };
        let Some(size) = ebml_size(data, &mut i) else { break };
        match size {
            Some(n) => {
                let n = n as usize;
                if i + n > data.len() {
                    break;
                }
                out.push((id, &data[i..i + n]));
                i += n;
            }
            None => {
                // Unknown size: treat the rest as the body (players do the same
                // for live-written segments).
                out.push((id, &data[i..]));
                break;
            }
        }
    }
    out
}

const EBML_SEGMENT: u64 = 0x1853_8067;
const EBML_INFO: u64 = 0x1549_a966;
const EBML_TIMECODE_SCALE: u64 = 0x002a_d7b1;
const EBML_DURATION: u64 = 0x0000_4489;
const EBML_TRACKS: u64 = 0x1654_ae6b;
const EBML_TRACK_ENTRY: u64 = 0xae;
const EBML_TRACK_TYPE: u64 = 0x83;
const EBML_CODEC_ID: u64 = 0x86;
const EBML_VIDEO: u64 = 0xe0;
const EBML_PIXEL_WIDTH: u64 = 0xb0;
const EBML_PIXEL_HEIGHT: u64 = 0xba;
const EBML_AUDIO: u64 = 0xe1;
const EBML_SAMPLING_FREQUENCY: u64 = 0xb5;
const EBML_CHANNELS: u64 = 0x9f;

fn ebml_info(data: &[u8], depth: usize, parts: &mut Parts) {
    if depth > 4 {
        return;
    }
    let mut timecode_scale = 1_000_000f64;
    let mut raw_duration = None;
    for (id, body) in ebml_elements(data) {
        match id {
            EBML_SEGMENT => ebml_info(body, depth + 1, parts),
            EBML_INFO => {
                for (id2, body2) in ebml_elements(body) {
                    match id2 {
                        EBML_TIMECODE_SCALE => timecode_scale = be_uint(body2) as f64,
                        EBML_DURATION => raw_duration = be_float(body2),
                        _ => {}
                    }
                }
            }
            EBML_TRACKS => {
                for (id2, body2) in ebml_elements(body) {
                    if id2 == EBML_TRACK_ENTRY {
                        ebml_track(body2, parts);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(d) = raw_duration
        && parts.duration.is_none()
    {
        // Duration is in timecode units; scale is nanoseconds per unit.
        parts.duration = Some(d * timecode_scale / 1e9);
    }
}

fn be_uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

fn be_float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
        8 => Some(f64::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

fn ebml_track(data: &[u8], parts: &mut Parts) {
    let mut track_type = 0u64;
    for (id, body) in ebml_elements(data) {
        match id {
            EBML_TRACK_TYPE => track_type = be_uint(body),
            EBML_CODEC_ID => {
                let codec = String::from_utf8_lossy(body);
                let name = matroska_codec(&codec);
                if track_type == 1 {
                    parts.video_codec.get_or_insert_with(|| name.clone());
                } else {
                    parts.audio_codec.get_or_insert_with(|| name.clone());
                }
            }
            EBML_VIDEO => {
                for (id2, body2) in ebml_elements(body) {
                    match id2 {
                        EBML_PIXEL_WIDTH => parts.width = Some(be_uint(body2) as u32),
                        EBML_PIXEL_HEIGHT => parts.height = Some(be_uint(body2) as u32),
                        _ => {}
                    }
                }
            }
            EBML_AUDIO => {
                for (id2, body2) in ebml_elements(body) {
                    match id2 {
                        EBML_SAMPLING_FREQUENCY => parts.sample_rate = be_float(body2).map(|f| f.round() as u32),
                        EBML_CHANNELS => parts.channels = Some(be_uint(body2) as u32),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    match track_type {
        1 => parts.has_video = true,
        2 => parts.has_audio = true,
        _ => {}
    }
}

fn matroska_codec(id: &str) -> String {
    let upper = id.to_ascii_uppercase();
    let name = if upper.contains("AVC") {
        "H.264"
    } else if upper.contains("HEVC") {
        "HEVC"
    } else if upper.contains("VP9") {
        "VP9"
    } else if upper.contains("VP8") {
        "VP8"
    } else if upper.contains("AV1") {
        "AV1"
    } else if upper.contains("OPUS") {
        "Opus"
    } else if upper.contains("VORBIS") {
        "Vorbis"
    } else if upper.contains("FLAC") {
        "FLAC"
    } else if upper.contains("AAC") {
        "AAC"
    } else if upper.contains("MPEG/L3") || upper.contains("MP3") {
        "MP3"
    } else if upper.contains("PCM") {
        "PCM"
    } else {
        return id.trim().to_string();
    };
    name.into()
}

// ---------------------------------------------------------------------------
// AVI
// ---------------------------------------------------------------------------

fn avi_scan(data: &[u8], depth: usize, parts: &mut Parts) {
    if depth > 4 {
        return;
    }
    for (id, body) in riff_chunks(data) {
        match &id {
            b"LIST" => {
                if body.len() >= 4 {
                    avi_scan(&body[4..], depth + 1, parts);
                }
            }
            b"avih" => {
                let micros = le32(body, 0).unwrap_or(0);
                let frames = le32(body, 16).unwrap_or(0);
                let w = le32(body, 32).unwrap_or(0);
                let h = le32(body, 36).unwrap_or(0);
                if w > 0 && h > 0 {
                    parts.width = Some(w);
                    parts.height = Some(h);
                }
                if micros > 0 {
                    parts.fps = Some(1e6 / micros as f64);
                    if frames > 0 {
                        parts.duration = Some(micros as f64 * frames as f64 / 1e6);
                    }
                }
            }
            b"strh" => {
                let ty = body.get(0..4).unwrap_or(b"    ");
                let handler = body.get(4..8).unwrap_or(b"    ");
                let scale = le32(body, 20).unwrap_or(0);
                let rate = le32(body, 24).unwrap_or(0);
                let length = le32(body, 32).unwrap_or(0);
                if ty == b"vids" {
                    parts.has_video = true;
                    let h = String::from_utf8_lossy(handler).trim().to_string();
                    if !h.is_empty() {
                        parts.video_codec.get_or_insert(h);
                    }
                    if parts.fps.is_none() && scale > 0 && rate > 0 {
                        parts.fps = Some(rate as f64 / scale as f64);
                    }
                    if parts.duration.is_none() && scale > 0 && rate > 0 && length > 0 {
                        parts.duration = Some(length as f64 * scale as f64 / rate as f64);
                    }
                } else if ty == b"auds" {
                    parts.has_audio = true;
                    if parts.duration.is_none() && scale > 0 && rate > 0 && length > 0 {
                        parts.duration = Some(length as f64 * scale as f64 / rate as f64);
                    }
                }
            }
            b"strf" => {
                // WAVEFORMATEX: format tag, channels, sample rate, byte rate...
                let channels = le16(body, 2).unwrap_or(0) as u32;
                let rate = le32(body, 4).unwrap_or(0);
                if channels > 0 {
                    parts.channels = Some(channels);
                }
                if rate > 0 {
                    parts.sample_rate = Some(rate);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

fn wav_info(head: &[u8], parts: &mut Parts) -> Result<(), PreviewError> {
    if !(head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE")) {
        return Err(PreviewError::Decode("not a RIFF/WAVE file".into()));
    }
    let mut data_len = None;
    let mut byte_rate = 0u32;
    for (id, body) in riff_chunks(&head[12..]) {
        match &id {
            b"fmt " => {
                let format = le16(body, 0).unwrap_or(0);
                let channels = le16(body, 2).unwrap_or(0) as u32;
                let rate = le32(body, 4).unwrap_or(0);
                byte_rate = le32(body, 8).unwrap_or(0);
                let bits = le16(body, 14).unwrap_or(0);
                parts.has_audio = true;
                parts.channels = Some(channels);
                parts.sample_rate = Some(rate);
                parts.audio_codec = Some(wav_codec(format, bits).to_string());
            }
            b"data" => data_len = Some(body.len() as u64),
            _ => {}
        }
        if data_len.is_some() && parts.sample_rate.is_some() {
            break;
        }
    }
    if parts.sample_rate.is_none() {
        return Err(PreviewError::Decode("no fmt chunk".into()));
    }
    if byte_rate > 0 {
        parts.bitrate = Some(byte_rate as f64 * 8.0);
        if let Some(len) = data_len {
            parts.duration = Some(len as f64 / byte_rate as f64);
        }
    }
    Ok(())
}

fn wav_codec(format: u16, bits: u16) -> &'static str {
    match format {
        1 => "PCM",
        3 => "IEEE float",
        6 => "A-law",
        7 => "µ-law",
        0x0055 => "MP3",
        0xfffe => "WAVE_FORMAT_EXTENSIBLE",
        _ if bits > 0 => "PCM",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// FLAC
// ---------------------------------------------------------------------------

fn flac_info(head: &[u8], file_size: u64, parts: &mut Parts) -> Result<(), PreviewError> {
    if !head.starts_with(b"fLaC") {
        return Err(PreviewError::Decode("not a FLAC file".into()));
    }
    let mut i = 4usize;
    while i + 4 <= head.len() {
        let last = head[i] & 0x80 != 0;
        let block_type = head[i] & 0x7f;
        let len = ((head[i + 1] as usize) << 16) | ((head[i + 2] as usize) << 8) | head[i + 3] as usize;
        let start = i + 4;
        if start + len > head.len() {
            break;
        }
        if block_type == 0 && len >= 34 {
            let b = &head[start..start + 34];
            let packed = be64(b, 10).unwrap_or(0);
            let sample_rate = ((packed >> 44) & 0x0f_ffff) as u32;
            let channels = (((packed >> 41) & 0x07) + 1) as u32;
            let bits = (((packed >> 36) & 0x1f) + 1) as u32;
            let total_samples = packed & 0x0000_000f_ffff_ffff;
            parts.has_audio = true;
            if sample_rate > 0 {
                parts.sample_rate = Some(sample_rate);
                if total_samples > 0 {
                    let secs = total_samples as f64 / sample_rate as f64;
                    parts.duration = Some(secs);
                    if secs > 0.0 {
                        parts.bitrate = Some(file_size as f64 * 8.0 / secs);
                    }
                }
            }
            parts.channels = Some(channels);
            parts.audio_codec = Some(format!("FLAC ({bits}-bit)"));
            break;
        }
        i = start + len;
        if last {
            break;
        }
    }
    if parts.sample_rate.is_none() {
        return Err(PreviewError::Decode("no STREAMINFO block".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MP3
// ---------------------------------------------------------------------------

const MP3_BITRATES_V1L3: [u32; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
const MP3_BITRATES_V1L2: [u32; 16] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0];
const MP3_BITRATES_V1L1: [u32; 16] = [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0];
const MP3_BITRATES_V2L3: [u32; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];
const MP3_SAMPLE_RATES: [[u32; 3]; 3] = [
    [44100, 48000, 32000], // MPEG 1
    [22050, 24000, 16000], // MPEG 2
    [11025, 12000, 8000],  // MPEG 2.5
];

fn mp3_info(head: &[u8], file_size: u64, parts: &mut Parts) -> Result<(), PreviewError> {
    let mut start = 0usize;
    if head.starts_with(b"ID3") {
        let size = ((head[6] as usize & 0x7f) << 21)
            | ((head[7] as usize & 0x7f) << 14)
            | ((head[8] as usize & 0x7f) << 7)
            | (head[9] as usize & 0x7f);
        start = 10 + size;
        if head.get(5).is_some_and(|f| f & 0x10 != 0) {
            start += 10; // footer
        }
    }
    let mut frame = None;
    let mut i = start;
    while i + 4 <= head.len() && i < start + 512 * 1024 {
        let (b0, b1, b2, b3) = (head[i], head[i + 1], head[i + 2], head[i + 3]);
        if b0 != 0xff || b1 & 0xe0 != 0xe0 {
            i += 1;
            continue;
        }
        let version_bits = (b1 >> 3) & 3; // 0 = 2.5, 2 = 2, 3 = 1
        let layer_bits = (b1 >> 1) & 3; // 1 = III, 2 = II, 3 = I
        let bitrate_idx = (b2 >> 4) as usize & 0xf;
        let rate_idx = (b2 >> 2) as usize & 3;
        let padding = (b2 >> 1) & 1;
        if version_bits == 1 || layer_bits == 0 || rate_idx == 3 || bitrate_idx == 0 || bitrate_idx == 15 {
            i += 1;
            continue;
        }
        let table = match (layer_bits, version_bits) {
            (3, 3) => &MP3_BITRATES_V1L1,
            (2, 3) => &MP3_BITRATES_V1L2,
            (1, 3) => &MP3_BITRATES_V1L3,
            (_, 3) => &MP3_BITRATES_V1L3,
            (1, _) => &MP3_BITRATES_V2L3,
            _ => &MP3_BITRATES_V2L3,
        };
        let kbps = table[bitrate_idx];
        if kbps == 0 {
            i += 1;
            continue;
        }
        let version_row = match version_bits {
            3 => 0,
            2 => 1,
            _ => 2,
        };
        let sample_rate = MP3_SAMPLE_RATES[version_row][rate_idx];
        let mono = (b3 >> 6) == 3;
        frame = Some((i, kbps, sample_rate, mono, layer_bits, version_bits, padding));
        break;
    }
    let Some((frame_at, kbps, sample_rate, mono, layer_bits, version_bits, padding)) = frame else {
        return Err(PreviewError::Decode("no MPEG audio frame".into()));
    };
    parts.has_audio = true;
    parts.sample_rate = Some(sample_rate);
    parts.channels = Some(if mono { 1 } else { 2 });
    parts.bitrate = Some(kbps as f64 * 1000.0);
    parts.audio_codec = Some(format!("MP3 (MPEG-{})", if version_bits == 3 { "1" } else { "2" }));
    let samples_per_frame = match layer_bits {
        3 => 384,
        2 => 1152,
        _ => 1152,
    };
    let _ = padding;
    // Prefer the Xing/Info frame count; otherwise estimate from the bitrate.
    let side_info = if version_bits == 3 { if mono { 17 } else { 32 } } else if mono { 9 } else { 17 };
    let xing_at = frame_at + 4 + side_info;
    let mut duration = None;
    if let Some(tag) = head.get(xing_at..xing_at + 4)
        && (tag == b"Xing" || tag == b"Info")
        && let Some(flags) = be32(head, xing_at + 4)
        && flags & 1 != 0
        && let Some(frames) = be32(head, xing_at + 8)
        && frames > 0
    {
        duration = Some(frames as f64 * samples_per_frame as f64 / sample_rate as f64);
    }
    parts.duration = duration.or_else(|| {
        let audio_bytes = file_size.saturating_sub(frame_at as u64);
        (kbps > 0).then(|| audio_bytes as f64 * 8.0 / (kbps as f64 * 1000.0))
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// OGG / Opus
// ---------------------------------------------------------------------------

fn ogg_info(path: &Path, file_size: u64, parts: &mut Parts) -> Result<(), PreviewError> {
    let mut file = File::open(path).map_err(|e| PreviewError::Io(format!("{}: {e}", path.display())))?;
    let head = read_at(&mut file, 0, 64 << 10)?;
    if !head.starts_with(b"OggS") {
        return Err(PreviewError::Decode("not an OGG file".into()));
    }
    let nsegs = *head.get(26).ok_or_else(|| PreviewError::Decode("short OGG page".into()))? as usize;
    let payload_at = 27 + nsegs;
    let payload = head.get(payload_at..).ok_or_else(|| PreviewError::Decode("short OGG page".into()))?;
    let mut sample_rate = None;
    if payload.starts_with(b"OpusHead") {
        parts.channels = payload.get(9).map(|c| *c as u32);
        sample_rate = le32(payload, 12);
        parts.audio_codec = Some("Opus".into());
    } else if payload.len() > 16 && payload[0] == 1 && &payload[1..7] == b"vorbis" {
        parts.channels = Some(payload[11] as u32);
        sample_rate = le32(payload, 12);
        parts.audio_codec = Some("Vorbis".into());
    } else if payload.starts_with(b"\x7fFLAC") {
        parts.audio_codec = Some("FLAC".into());
    } else {
        return Err(PreviewError::Decode("unknown OGG codec".into()));
    }
    parts.has_audio = true;
    // The last page's granule position is the sample count.
    let tail_len = file_size.min(64 << 10) as usize;
    let tail = read_at(&mut file, file_size - tail_len as u64, tail_len)?;
    let granule = tail
        .windows(4)
        .rposition(|w| w == b"OggS")
        .and_then(|at| le64(&tail, at + 6));
    if let (Some(rate), Some(g)) = (sample_rate, granule)
        && rate > 0
        && g > 0
    {
        parts.sample_rate = Some(rate);
        parts.duration = Some(g as f64 / rate as f64);
    } else if let Some(rate) = sample_rate {
        parts.sample_rate = Some(rate);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsubame-preview-media-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn row<'a>(info: &'a MediaInfo, key: &str) -> Option<&'a str> {
        info.rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn wav_bytes(rate: u32, channels: u16, bits: u16, data_len: u32) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&block_align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&data_len.to_le_bytes());
        body.extend(std::iter::repeat_n(0u8, data_len as usize));
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn wav_metadata_comes_from_the_fmt_chunk() {
        let d = temp_dir("wav");
        let p = d.join("a.wav");
        std::fs::write(&p, wav_bytes(8_000, 1, 8, 8_000)).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Audio);
        assert_eq!(row(&info, "Duration"), Some("0:01"));
        assert_eq!(row(&info, "Sample rate"), Some("8 kHz"));
        assert_eq!(row(&info, "Channels"), Some("Mono"));
        assert_eq!(row(&info, "Bitrate"), Some("64 kbps"));
        assert_eq!(row(&info, "Audio codec"), Some("PCM"));
        assert_eq!(row(&info, "Container"), Some("WAV"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn mp4_box(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(body);
        out
    }

    fn mp4_mvhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut b = vec![0u8; 100];
        b[12..16].copy_from_slice(&timescale.to_be_bytes());
        b[16..20].copy_from_slice(&duration.to_be_bytes());
        mp4_box(b"mvhd", &b)
    }

    fn mp4_track(handler: &[u8; 4], width: u32, height: u32, rate: u32, fourcc: &[u8; 4]) -> Vec<u8> {
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(width << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(height << 16).to_be_bytes());
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&rate.to_be_bytes());
        mdhd[16..20].copy_from_slice(&rate.to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(handler);
        let mut entry = vec![0u8; 40];
        entry[..4].copy_from_slice(&40u32.to_be_bytes());
        entry[4..8].copy_from_slice(fourcc);
        if handler == b"soun" {
            entry[24..26].copy_from_slice(&2u16.to_be_bytes());
            entry[32..36].copy_from_slice(&(48_000u32 << 16).to_be_bytes());
        }
        let mut stsd = vec![0u8; 8];
        stsd[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd.extend(entry);
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let minf = mp4_box(b"minf", &stbl);
        let mut mdia = Vec::new();
        mdia.extend(mp4_box(b"mdhd", &mdhd));
        mdia.extend(mp4_box(b"hdlr", &hdlr));
        mdia.extend(minf);
        let mut trak = Vec::new();
        trak.extend(mp4_box(b"tkhd", &tkhd));
        trak.extend(mp4_box(b"mdia", &mdia));
        mp4_box(b"trak", &trak)
    }

    #[test]
    fn mp4_metadata_comes_from_moov() {
        let d = temp_dir("mp4");
        let p = d.join("a.mp4");
        let mut moov = mp4_mvhd(1000, 5000);
        moov.extend(mp4_track(b"vide", 1920, 1080, 1000, b"avc1"));
        moov.extend(mp4_track(b"soun", 0, 0, 5000, b"mp4a"));
        let mut file = mp4_box(b"ftyp", b"isom\0\0\0\0");
        file.extend(mp4_box(b"moov", &moov));
        file.extend(mp4_box(b"mdat", &[0u8; 16]));
        std::fs::write(&p, &file).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Video);
        assert_eq!(row(&info, "Duration"), Some("0:05"));
        assert_eq!(row(&info, "Resolution"), Some("1920 × 1080"));
        assert_eq!(row(&info, "Video codec"), Some("H.264"));
        assert_eq!(row(&info, "Audio codec"), Some("AAC"));
        assert_eq!(row(&info, "Sample rate"), Some("48 kHz"));
        assert_eq!(row(&info, "Channels"), Some("Stereo"));
        assert_eq!(row(&info, "Container"), Some("MP4"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mp4_handles_a_moov_at_the_end() {
        let d = temp_dir("mp4-tail");
        let p = d.join("b.m4a");
        let mut moov = mp4_mvhd(1000, 60_000);
        moov.extend(mp4_track(b"soun", 0, 0, 60_000, b"mp4a"));
        let mut file = mp4_box(b"ftyp", b"M4A \0\0\0\0");
        file.extend(mp4_box(b"mdat", &[0u8; 1024]));
        file.extend(mp4_box(b"moov", &moov));
        std::fs::write(&p, &file).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Audio);
        assert_eq!(row(&info, "Duration"), Some("1:00"));
        assert_eq!(row(&info, "Audio codec"), Some("AAC"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn flac_bytes(rate: u32, channels: u32, bits: u32, total: u64) -> Vec<u8> {
        let mut streaminfo = vec![0u8; 34];
        streaminfo[0..2].copy_from_slice(&4096u16.to_be_bytes());
        streaminfo[2..4].copy_from_slice(&4096u16.to_be_bytes());
        let packed = ((rate as u64) << 44) | (((channels - 1) as u64) << 41) | (((bits - 1) as u64) << 36) | total;
        streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());
        let mut out = b"fLaC".to_vec();
        out.push(0x80); // last block, type 0 (STREAMINFO)
        out.extend_from_slice(&[0, 0, 34]);
        out.extend(streaminfo);
        out
    }

    #[test]
    fn flac_metadata_comes_from_streaminfo() {
        let d = temp_dir("flac");
        let p = d.join("a.flac");
        std::fs::write(&p, flac_bytes(44_100, 2, 16, 44_100)).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Audio);
        assert_eq!(row(&info, "Duration"), Some("0:01"));
        assert_eq!(row(&info, "Sample rate"), Some("44.1 kHz"));
        assert_eq!(row(&info, "Channels"), Some("Stereo"));
        assert_eq!(row(&info, "Audio codec"), Some("FLAC (16-bit)"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn mp3_bytes(kbps_index: u8) -> Vec<u8> {
        // MPEG 1 Layer III, 44.1 kHz, stereo
        let mut frame = vec![0xff, 0xfb, kbps_index << 4, 0x00];
        frame.extend(std::iter::repeat_n(0u8, 4_000));
        frame
    }

    #[test]
    fn mp3_metadata_comes_from_the_first_frame() {
        let d = temp_dir("mp3");
        let p = d.join("a.mp3");
        std::fs::write(&p, mp3_bytes(9)).unwrap(); // 9 = 128 kbps
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Audio);
        assert_eq!(row(&info, "Bitrate"), Some("128 kbps"));
        assert_eq!(row(&info, "Sample rate"), Some("44.1 kHz"));
        assert_eq!(row(&info, "Channels"), Some("Stereo"));
        assert!(row(&info, "Duration").is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        assert!(body.len() < 127, "test sizes stay one-byte varints");
        out.push(0x80 | body.len() as u8);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn matroska_metadata_comes_from_info_and_tracks() {
        let d = temp_dir("mkv");
        let p = d.join("a.mkv");
        let mut info_body = ebml(&[0x2a, 0xd7, 0xb1], &1_000_000u32.to_be_bytes());
        info_body.extend(ebml(&[0x44, 0x89], &12_345.0f32.to_be_bytes()));
        let info_el = ebml(&[0x15, 0x49, 0xa9, 0x66], &info_body);
        let mut video = ebml(&[0x83], &[1]);
        video.extend(ebml(&[0x86], b"V_MPEG4/ISO/AVC"));
        let mut vbody = ebml(&[0xb0], &1920u16.to_be_bytes());
        vbody.extend(ebml(&[0xba], &1080u16.to_be_bytes()));
        video.extend(ebml(&[0xe0], &vbody));
        let mut tracks_body = ebml(&[0xae], &video);
        let mut audio = ebml(&[0x83], &[2]);
        audio.extend(ebml(&[0x86], b"A_OPUS"));
        let mut abody = ebml(&[0xb5], &48_000.0f32.to_be_bytes());
        abody.extend(ebml(&[0x9f], &[2]));
        audio.extend(ebml(&[0xe1], &abody));
        tracks_body.extend(ebml(&[0xae], &audio));
        let tracks_el = ebml(&[0x16, 0x54, 0xae, 0x6b], &tracks_body);
        let mut segment = info_el;
        segment.extend(tracks_el);
        let mut file = vec![0x1a, 0x45, 0xdf, 0xa3];
        // The EBML header can stay empty; the segment is what matters.
        file.push(0x80);
        file.extend(ebml(&[0x18, 0x53, 0x80, 0x67], &segment));
        std::fs::write(&p, &file).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Video);
        assert_eq!(row(&info, "Duration"), Some("0:12"));
        assert_eq!(row(&info, "Resolution"), Some("1920 × 1080"));
        assert_eq!(row(&info, "Video codec"), Some("H.264"));
        assert_eq!(row(&info, "Audio codec"), Some("Opus"));
        assert_eq!(row(&info, "Sample rate"), Some("48 kHz"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn avi_metadata_comes_from_the_header_list() {
        let d = temp_dir("avi");
        let p = d.join("a.avi");
        let mut avih = vec![0u8; 56];
        avih[0..4].copy_from_slice(&40_000u32.to_le_bytes()); // 25 fps
        avih[16..20].copy_from_slice(&250u32.to_le_bytes()); // frames
        avih[32..36].copy_from_slice(&640u32.to_le_bytes());
        avih[36..40].copy_from_slice(&480u32.to_le_bytes());
        let mut hdrl = b"hdrl".to_vec();
        hdrl.extend_from_slice(b"avih");
        hdrl.extend_from_slice(&(avih.len() as u32).to_le_bytes());
        hdrl.extend_from_slice(&avih);
        let mut strh = vec![0u8; 56];
        strh[0..4].copy_from_slice(b"vids");
        strh[4..8].copy_from_slice(b"H264");
        strh[20..24].copy_from_slice(&1u32.to_le_bytes());
        strh[24..28].copy_from_slice(&25u32.to_le_bytes());
        strh[32..36].copy_from_slice(&250u32.to_le_bytes());
        hdrl.extend_from_slice(b"strh");
        hdrl.extend_from_slice(&(strh.len() as u32).to_le_bytes());
        hdrl.extend_from_slice(&strh);
        let mut list = b"LIST".to_vec();
        list.extend_from_slice(&(hdrl.len() as u32).to_le_bytes());
        list.extend_from_slice(&hdrl);
        let mut body = b"AVI ".to_vec();
        body.extend_from_slice(&list);
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        std::fs::write(&p, &file).unwrap();
        let info = load(&p).unwrap();
        assert_eq!(info.kind, MediaKind::Video);
        assert_eq!(row(&info, "Resolution"), Some("640 × 480"));
        assert_eq!(row(&info, "Frame rate"), Some("25.00 fps"));
        assert_eq!(row(&info, "Duration"), Some("0:10"));
        assert_eq!(row(&info, "Video codec"), Some("H264"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn damaged_and_unknown_files_are_errors_not_panics() {
        let d = temp_dir("bad");
        let p = d.join("a.mp4");
        std::fs::write(&p, b"not an mp4 at all").unwrap();
        assert!(load(&p).is_err());
        let q = d.join("a.mkv");
        std::fs::write(&q, [0x1a, 0x45, 0xdf, 0xa3, 0x80]).unwrap();
        assert!(load(&q).is_err());
        let r = d.join("a.wav");
        std::fs::write(&r, b"RIFF\0\0\0\0WAVE").unwrap();
        assert!(load(&r).is_err());
        let s = d.join("a.xyz");
        std::fs::write(&s, b"whatever").unwrap();
        assert!(matches!(load(&s), Err(PreviewError::Unsupported(_))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn formatters_read_naturally() {
        assert_eq!(format_duration(0.4), "0:00");
        assert_eq!(format_duration(59.6), "1:00");
        assert_eq!(format_duration(225.0), "3:45");
        assert_eq!(format_duration(5025.0), "1:23:45");
        assert_eq!(format_bitrate(1_411_200.0), "1.4 Mbps");
        assert_eq!(format_bitrate(320_000.0), "320 kbps");
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(48_000), "48 kHz");
        assert_eq!(format_sample_rate(7_350), "7350 Hz");
        assert_eq!(format_channels(1), "Mono");
        assert_eq!(format_channels(6), "5.1");
        assert_eq!(format_channels(4), "4 channels");
    }
}
