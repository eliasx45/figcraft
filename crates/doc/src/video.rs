//! A video file on a layer: the file (linked, not embedded: a library page may hold hundreds of
//! clips), what it holds, and the stretch of it the card plays. The desktop app decodes the
//! pictures (see `figcraft-video`); the layer's own fill shows wherever it can't.

use serde::{Deserialize, Serialize};

/// Longest path kept.
pub const MAX_TEXT: usize = 4096;
/// Longest video accepted (seconds).
pub const MAX_DURATION: f64 = 24.0 * 3600.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoClip {
    /// The video file (absolute path).
    pub path: String,
    /// Length of the whole file in seconds.
    pub duration: f64,
    /// The picture's size in pixels.
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: f64,
    /// The stretch played, in media seconds.
    pub start: f64,
    pub end: f64,
    /// The file has sound, and the card plays it (when started with its play button).
    #[serde(default = "yes")]
    pub sound: bool,
    /// Start again from `start` at the end (off by default: a card plays once and stops).
    #[serde(default, rename = "loop")]
    pub looping: bool,
}

fn default_fps() -> f64 {
    30.0
}
fn yes() -> bool {
    true
}

impl VideoClip {
    pub fn new(path: impl Into<String>, duration: f64, width: u32, height: u32, fps: f64) -> Self {
        let mut v = Self { path: path.into(), duration, width, height, fps, start: 0.0, end: duration, sound: true, looping: false };
        v.sanitize();
        v
    }

    /// Keep every value usable: finite, ordered, bounded.
    pub fn sanitize(&mut self) {
        crate::audio::truncate_chars(&mut self.path, MAX_TEXT);
        if !(self.duration.is_finite() && self.duration > 0.0) {
            self.duration = 1.0;
        }
        self.duration = self.duration.min(MAX_DURATION);
        if !(self.fps.is_finite() && (0.01..=1000.0).contains(&self.fps)) {
            self.fps = default_fps();
        }
        self.width = self.width.clamp(1, 16_384);
        self.height = self.height.clamp(1, 16_384);
        if !self.start.is_finite() {
            self.start = 0.0;
        }
        if !self.end.is_finite() {
            self.end = self.duration;
        }
        self.start = self.start.clamp(0.0, self.duration);
        self.end = self.end.clamp(0.0, self.duration);
        if self.end <= self.start {
            self.end = (self.start + 1.0 / self.fps).min(self.duration);
            self.start = self.start.min((self.end - 1.0 / self.fps).max(0.0));
        }
    }

    /// Seconds played.
    pub fn span(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// The frame number at media second `t`.
    pub fn frame_of(&self, t: f64) -> i64 {
        let f = (t * self.fps).floor();
        if f.is_finite() { f.clamp(-1e12, 1e12) as i64 } else { 0 }
    }

    /// The file's name, for labels.
    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_stay_usable() {
        let mut v = VideoClip::new("/clips/a.mp4", 18.0, 1280, 720, 29.97);
        assert_eq!((v.start, v.end), (0.0, 18.0));
        v.start = 20.0;
        v.end = f64::NAN;
        v.sanitize();
        assert!(v.start < v.end && v.end <= 18.0, "{v:?}");
        let w = VideoClip::new("x.mov", f64::INFINITY, 0, 0, -1.0);
        assert_eq!((w.duration, w.width, w.fps), (1.0, 1, 30.0));
        assert_eq!(VideoClip::new("/a/b/c d.mp4", 1.0, 2, 2, 30.0).file_name(), "c d.mp4");
    }
}
