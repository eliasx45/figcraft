//! A live EffectCraft comp on a layer: the comp, the stretch of its time the layer shows, and
//! where that sits on the edit's timeline. The pictures come from the running EffectCraft app
//! (the desktop app streams them in); the layer's fill holds a poster frame for when it isn't
//! running, for exports and for the browser build.

use serde::{Deserialize, Serialize};

/// Longest comp name / project path kept.
pub const MAX_TEXT: usize = 1024;
/// Longest stretch of comp time a card shows (seconds).
pub const MAX_SPAN: f64 = 3600.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EcComp {
    /// The comp's name in EffectCraft (how it is found again when ids changed).
    pub comp: String,
    /// The comp's item id in the project it came from (0: unknown).
    #[serde(default)]
    pub comp_id: u64,
    /// The `.ecproj` it came from (empty: unknown). Informational: the card plays from whatever
    /// project EffectCraft has open.
    #[serde(default)]
    pub project: String,
    /// The stretch shown, in comp seconds.
    pub start: f64,
    pub end: f64,
    /// The comp's frame rate.
    #[serde(default = "default_fps")]
    pub fps: f64,
    /// The comp's size in pixels.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Timeline seconds of comp time 0 (a precomp starting 1.4 s into MAIN has 1.4), so cards
    /// and audio strips of the same edit share one playhead.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// Comp seconds per timeline second (a precomp layer stretched to 200 % has 0.5).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub rate: f64,
    /// Play the comp's own sound.
    #[serde(default = "yes")]
    pub sound: bool,
}

fn default_fps() -> f64 {
    29.97
}
fn one() -> f64 {
    1.0
}
fn yes() -> bool {
    true
}
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}
fn is_one(v: &f64) -> bool {
    *v == 1.0
}

impl EcComp {
    pub fn new(comp: impl Into<String>, start: f64, end: f64, fps: f64) -> Self {
        let mut c = Self { comp: comp.into(), comp_id: 0, project: String::new(), start, end, fps, width: 1920, height: 1080, offset: 0.0, rate: 1.0, sound: true };
        c.sanitize();
        c
    }

    /// Keep every value usable: finite, ordered, bounded.
    pub fn sanitize(&mut self) {
        crate::audio::truncate_chars(&mut self.comp, MAX_TEXT);
        crate::audio::truncate_chars(&mut self.project, MAX_TEXT);
        if !(self.fps.is_finite() && (0.01..=1000.0).contains(&self.fps)) {
            self.fps = default_fps();
        }
        if !self.start.is_finite() {
            self.start = 0.0;
        }
        self.start = self.start.clamp(-MAX_SPAN * 24.0, MAX_SPAN * 24.0);
        if !(self.end.is_finite() && self.end > self.start) {
            self.end = self.start + 1.0 / self.fps;
        }
        self.end = self.end.min(self.start + MAX_SPAN);
        if !self.offset.is_finite() {
            self.offset = 0.0;
        }
        if !(self.rate.is_finite() && self.rate > 0.0) {
            self.rate = 1.0;
        }
        self.rate = self.rate.clamp(0.01, 100.0);
        self.width = self.width.clamp(1, 32_768);
        self.height = self.height.clamp(1, 32_768);
    }

    /// Seconds of comp time shown.
    pub fn span(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// The comp frame at comp second `t`.
    pub fn frame_of(&self, t: f64) -> i64 {
        let f = (t * self.fps + 1e-6).floor();
        if f.is_finite() { f.clamp(-1e12, 1e12) as i64 } else { 0 }
    }

    /// The comp frames shown, first to last.
    pub fn frames(&self) -> std::ops::RangeInclusive<i64> {
        let a = self.frame_of(self.start);
        let b = self.frame_of(self.end - 0.5 / self.fps).max(a);
        a..=b
    }

    /// Timeline seconds of comp second `t`.
    pub fn timeline_of(&self, t: f64) -> f64 {
        self.offset + t / self.rate
    }

    /// Comp seconds of timeline second `t`.
    pub fn comp_time_of(&self, t: f64) -> f64 {
        (t - self.offset) * self.rate
    }

    /// Whether timeline second `t` falls in the stretch shown.
    pub fn covers_timeline(&self, t: f64) -> bool {
        let c = self.comp_time_of(t);
        c >= self.start && c < self.end
    }
}
