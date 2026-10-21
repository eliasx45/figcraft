//! Video files on the canvas: what a file holds ([`probe`]), and its frames, decoded in order from
//! a time and scaled to the size the card shows ([`Reader`]); a poster is the first of them.
//!
//! On macOS AVFoundation reads the file and the Apple media engine decodes H.264, HEVC and ProRes,
//! so many small cards can play at once for little CPU. Elsewhere every call answers
//! [`Error::Unsupported`] and the card shows its name.
//!
//! The FFI lives in one module (`apple`), the only `unsafe` in FigCraft (docs/adr/0001-video-ffi.md).

use std::fmt;
use std::path::Path;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;
#[cfg(test)]
mod tests;

/// File extensions treated as video (a `.mov` or `.mp4` without a picture still imports as sound).
pub const EXTENSIONS: &[&str] = &["mp4", "mov", "m4v"];
/// Longest side of a decoded frame (pixels): cards never need more.
pub const MAX_SIDE: u32 = 1920;

/// Whether `name` has a video file's extension.
pub fn is_video_name(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// What a video file holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// Seconds.
    pub duration: f64,
    /// The picture's size (pixels).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// It has a sound track.
    pub sound: bool,
}

/// One decoded frame: RGBA8, not premultiplied, at media time `time` (seconds).
#[derive(Clone, Debug)]
pub struct Frame {
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No video decoding on this system.
    Unsupported,
    /// The file can't be opened or has no picture.
    Open(String),
    /// Decoding stopped.
    Decode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => write!(f, "video playback isn't available on this system yet"),
            Error::Open(e) => write!(f, "can't open the video: {e}"),
            Error::Decode(e) => write!(f, "can't decode the video: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The size a `w`×`h` picture is decoded at for a longest side of at most `max_side` (never
/// larger than it is; even sides, at least 2).
pub fn fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let max_side = max_side.clamp(2, MAX_SIDE);
    let long = w.max(h);
    let s = if long > max_side { f64::from(max_side) / f64::from(long) } else { 1.0 };
    let even = |v: u32| -> u32 {
        let x = (f64::from(v) * s).round().max(2.0) as u32;
        (x & !1).max(2)
    };
    (even(w), even(h))
}

/// What the video file at `path` holds.
pub fn probe(path: &Path) -> Result<Info, Error> {
    #[cfg(target_os = "macos")]
    return apple::probe(path);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(Error::Unsupported)
    }
}

/// Frames of a video file in order from a time, scaled down to a longest side. Not `Send`: make
/// it on the thread that reads it.
pub struct Reader {
    #[cfg(target_os = "macos")]
    inner: apple::Reader,
}

impl Reader {
    /// Read `path` from media second `from` up to `until` (the end when infinite), frames at most
    /// `max_side` pixels on their longest side.
    pub fn open(path: &Path, from: f64, until: f64, max_side: u32) -> Result<Reader, Error> {
        let from = if from.is_finite() { from.max(0.0) } else { 0.0 };
        #[cfg(target_os = "macos")]
        return Ok(Reader { inner: apple::Reader::open(path, from, until, max_side)? });
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (path, from, until, max_side);
            Err(Error::Unsupported)
        }
    }

    /// The next frame, or `None` at the end (or on a decoding error).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Frame> {
        #[cfg(target_os = "macos")]
        return self.inner.next();
        #[cfg(not(target_os = "macos"))]
        None
    }
}

/// The frame of `path` at media second `t` (the first one at or after it), at most `max_side`
/// pixels on its longest side.
pub fn frame_at(path: &Path, t: f64, max_side: u32) -> Result<Frame, Error> {
    let mut r = Reader::open(path, t, f64::INFINITY, max_side)?;
    r.next().ok_or_else(|| Error::Decode("no frame there".into()))
}
