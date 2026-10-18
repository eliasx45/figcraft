//! Audio on the canvas: a layer can carry an audio clip ([`AudioClip`]), drawn as a waveform strip
//! with a time ruler and markers. The sound itself is kept once per file by content hash in
//! [`Document::audio`](crate::Document::audio); the clip holds what the renderer needs without
//! decoding it (a peak envelope) and what agents set (the range shown, the ruler's frame rate,
//! markers).

use figcraft_color::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

/// The most markers one clip keeps.
pub const MAX_MARKERS: usize = 2000;
/// The longest marker label (characters).
pub const MAX_LABEL: usize = 200;
/// The most peak buckets one clip keeps (about 2.7 hours at 200 per second).
pub const MAX_PEAKS: usize = 2_000_000;

/// Encoded audio bytes (WAV/MP3/AAC/M4A/…) kept by content hash.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioData {
    pub bytes: Arc<Vec<u8>>,
}

/// A time-stamped label on an audio clip (a stressed syllable, a beat, an animation peak).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMarker {
    /// Media time in seconds (0 = the start of the file).
    pub time: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// `None`: the clip's marker colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

/// The loudest sample of each slice of the sound, 0..=255 (full scale), at
/// [`AudioClip::peak_rate`] slices per second: enough to draw the waveform at any zoom without the
/// decoder. Saved as base64.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peaks(pub Arc<Vec<u8>>);

impl Serialize for Peaks {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(self.0.as_slice()))
    }
}

impl<'de> Deserialize<'de> for Peaks {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        // A damaged envelope draws as silence rather than failing the file.
        let mut v = base64::engine::general_purpose::STANDARD.decode(s.trim()).unwrap_or_default();
        v.truncate(MAX_PEAKS);
        Ok(Peaks(Arc::new(v)))
    }
}

/// An audio clip on a layer: which sound, which stretch of it the layer shows (its width is that
/// stretch), how the ruler counts, and the markers on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioClip {
    /// The sound's content hash (a key of [`Document::audio`](crate::Document::audio)).
    pub asset: String,
    /// The imported file's name (shown, and a hint for the decoder).
    #[serde(default)]
    pub file_name: String,
    /// Length of the whole sound in seconds.
    pub duration: f64,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub channels: u16,
    /// The stretch shown, in media seconds: `start` at the layer's left edge, `end` at its right.
    pub start: f64,
    pub end: f64,
    /// Frames per second the ruler counts in (29.97, 25, 30…).
    #[serde(default = "default_fps")]
    pub fps: f64,
    /// Timeline time of the sound's first sample, in seconds: the ruler and marker frames read
    /// `offset + media time` (a clip that starts 42 s into the edit shows frame 1259 at media 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// Waveform colour.
    #[serde(default = "default_wave")]
    pub color: Color,
    /// Marker colour (each marker may have its own).
    #[serde(default = "default_marker")]
    pub marker_color: Color,
    /// Draw the time ruler along the top.
    #[serde(default = "crate::node::yes")]
    pub ruler: bool,
    /// Peak slices per second.
    #[serde(default)]
    pub peak_rate: u32,
    #[serde(default)]
    pub peaks: Peaks,
    /// Sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<AudioMarker>,
}

fn default_fps() -> f64 {
    29.97
}
fn default_wave() -> Color {
    Color::from_rgb8_const(0x4F, 0xC3, 0xA1)
}
fn default_marker() -> Color {
    Color::from_rgb8_const(0xFF, 0xB0, 0x20)
}
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl AudioClip {
    /// A clip showing the whole of a `duration`-second sound.
    pub fn new(asset: impl Into<String>, file_name: impl Into<String>, duration: f64, sample_rate: u32, channels: u16) -> Self {
        let duration = if duration.is_finite() { duration.max(0.0) } else { 0.0 };
        Self {
            asset: asset.into(),
            file_name: file_name.into(),
            duration,
            sample_rate,
            channels,
            start: 0.0,
            end: duration,
            fps: default_fps(),
            offset: 0.0,
            color: default_wave(),
            marker_color: default_marker(),
            ruler: true,
            peak_rate: 0,
            peaks: Peaks::default(),
            markers: Vec::new(),
        }
    }

    /// Seconds shown (never zero, so positions divide safely).
    pub fn span(&self) -> f64 {
        let s = self.end - self.start;
        if s.is_finite() && s > 1e-9 { s } else { 1e-9 }
    }

    /// The x (layer units, 0..`width`) of media time `t`.
    pub fn x_of(&self, t: f64, width: f64) -> f64 {
        (t - self.start) / self.span() * width
    }

    /// The media time at x (layer units) for a layer `width` wide.
    pub fn time_at(&self, x: f64, width: f64) -> f64 {
        if width <= 0.0 { self.start } else { self.start + x / width * self.span() }
    }

    /// The ruler frame of media time `t` (timeline frames: `offset` included), rounded.
    pub fn frame_of(&self, t: f64) -> i64 {
        let f = ((t + self.offset) * self.fps).round();
        if f.is_finite() { f.clamp(-1e15, 1e15) as i64 } else { 0 }
    }

    /// The media time of ruler frame `frame`.
    pub fn time_of_frame(&self, frame: f64) -> f64 {
        if self.fps > 0.0 { frame / self.fps - self.offset } else { -self.offset }
    }

    /// The peak (0..=1) of the slice holding media time `t`, the loudest over `[t0, t1)`.
    pub fn peak_over(&self, t0: f64, t1: f64) -> f32 {
        let rate = f64::from(self.peak_rate);
        let p = &self.peaks.0;
        if rate <= 0.0 || p.is_empty() || !(t0.is_finite() && t1.is_finite()) {
            return 0.0;
        }
        let a = (t0 * rate).floor().max(0.0) as usize;
        let b = ((t1 * rate).ceil().max(0.0) as usize).max(a + 1).min(p.len());
        p.get(a..b).and_then(|s| s.iter().max()).map_or(0.0, |v| f32::from(*v) / 255.0)
    }

    /// Fix what a hand-edited or damaged clip may hold: non-finite or reversed times, a
    /// non-positive frame rate, too many or unsorted markers.
    pub fn sanitize(&mut self) {
        let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
        self.duration = fin(self.duration, 0.0).max(0.0);
        self.start = fin(self.start, 0.0);
        self.end = fin(self.end, self.duration);
        if self.end <= self.start {
            self.end = self.start + self.duration.max(1.0);
        }
        self.fps = fin(self.fps, default_fps());
        if !(0.01..=1000.0).contains(&self.fps) {
            self.fps = default_fps();
        }
        self.offset = fin(self.offset, 0.0);
        self.markers.retain(|m| m.time.is_finite());
        self.markers.truncate(MAX_MARKERS);
        for m in &mut self.markers {
            truncate_chars(&mut m.label, MAX_LABEL);
        }
        self.sort_markers();
    }

    pub fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    }
}

/// Cut `s` to at most `max` characters (at a char boundary).
pub fn truncate_chars(s: &mut String, max: usize) {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
    }
}
