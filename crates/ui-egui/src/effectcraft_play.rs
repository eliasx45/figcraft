//! Fork: a playing EffectCraft card plays the way EffectCraft's viewer previews (Space with Cache
//! Before Playback off): frames are rendered ahead of the playhead, a frame that isn't ready yet
//! is never waited for and never shown black (the newest frame ready, at or after the one on
//! screen, stays up: frames are dropped to keep time), and EffectCraft's own `Playback` meter
//! says whether the card keeps real time (its Info panel readout, on a green or red chip).
//!
//! Elias, 29 Oct (the "MAIN · whole video" card of the 26-10 board): black frames after about
//! 3 s and a low, uneven rate. The fetch then asked for the frames nearest the playhead on both
//! sides (half of them behind it, already played) in batches of 24, two at a time; EffectCraft
//! answers a batch only once all 24 are rendered, so by then the playhead had passed most of them,
//! and once it ran more than [`super::NEAR`] (2 s) past the newest frame held, the card found no
//! frame "near" the playhead and drew nothing over its dark fill. Now ([`order`]): only frames
//! ahead, on the preview's Skip grid, from where the playhead will be when they are ready (the
//! measured time from asking for a frame to having it, [`Meter::lead`]), in small batches; and the card holds the newest frame
//! ready ([`super::Link::texture_playing`]).
//!
//! The preview's rules (range, loop, frame rate, Skip, resolution, Cache Before Playback) are
//! EffectCraft's Space preset, already applied to the card's transport
//! (`figcraft_engine::effectcraft::Transport::span`) and its frame size ([`super::cards`]).

use std::cell::RefCell;

use effectcraft_ui_egui::Playback;
use figcraft_doc::{EcComp, NodeId};
use figcraft_ec_inproc::effectcraft_engine::preview::PreviewPlan;
use figcraft_ec_inproc::effectcraft_engine::time::Tick;
use figcraft_engine::effectcraft::Span;

/// Frames asked for in one request while a card plays (EffectCraft answers a request once all of
/// it is rendered: small batches land sooner).
pub(crate) const PLAY_BATCH: usize = 4;
/// Requests a playing card keeps out at once, so a batch is always rendering.
pub(crate) const PLAY_IN_FLIGHT: usize = 2;
/// The time from asking for a frame to having it, assumed before one was measured (seconds).
const FIRST_GUESS: f64 = 0.25;
/// How fast the measured time follows a new measurement (0..1).
const EMA: f64 = 0.3;
/// Most frames whose ask time is remembered.
const MAX_ASKED: usize = 512;
/// Most seconds ahead of the playhead a batch starts (a very slow comp: never lose sight of it).
const MAX_LEAD: f64 = 4.0;

/// What the playing card's frames cost and what it showed.
#[derive(Default)]
pub(crate) struct Meter {
    /// The card and the transport's generation it measures (a new play, pause or seek starts over).
    of: Option<(NodeId, u64)>,
    /// Seconds from asking for a frame to having it (moving average), and when each frame still
    /// coming was first asked for.
    latency: Option<f64>,
    asked: std::collections::HashMap<i64, f64>,
    /// The frame on screen (comp frame) and EffectCraft's meter of the frames shown.
    last: Option<i64>,
    pub(crate) playback: Playback,
}

impl Meter {
    /// Start over for card `card` at transport generation `generation` playing at `fps` comp
    /// frames a second every `step` frames, unless that is what it measures already.
    pub(crate) fn follow(&mut self, card: NodeId, generation: u64, fps: f64, step: i64) {
        if self.of == Some((card, generation)) {
            return;
        }
        self.of = Some((card, generation));
        self.last = None;
        self.asked.clear();
        self.playback = Playback { playing: true, fps: if fps.is_finite() && fps > 0.0 { fps } else { 30.0 }, ..Playback::default() };
        // EffectCraft's meter counts the frames a Skip means to show: only the plan's step is read.
        self.playback.plan = Some(PreviewPlan {
            start: 0,
            end: 0,
            first: 0,
            step: step.max(1),
            fps: self.playback.fps,
            looping: true,
            cache_first: false,
            video: true,
            audio: true,
            origin: Tick(0),
        });
    }

    /// Nothing plays.
    pub(crate) fn stop(&mut self) {
        self.of = None;
        self.last = None;
        self.asked.clear();
        self.playback = Playback::default();
    }

    /// The card it measures.
    pub(crate) fn card(&self) -> Option<NodeId> {
        self.of.map(|(c, _)| c)
    }

    /// The frame on screen now (comp frame) went up at `now`.
    pub(crate) fn shown(&mut self, frame: i64, now: f64) {
        if self.last != Some(frame) {
            self.last = Some(frame);
            self.playback.frame_shown(now, frame);
        }
        self.playback.observe(now);
    }

    /// The frame on screen, if one went up since play started.
    pub(crate) fn last(&self) -> Option<i64> {
        self.last
    }

    /// `frames` were asked for at `now` (the first ask of each counts).
    pub(crate) fn ask(&mut self, frames: &[i64], now: f64) {
        if self.asked.len() > MAX_ASKED {
            self.asked.clear();
        }
        for f in frames {
            self.asked.entry(*f).or_insert(now);
        }
    }

    /// `frames` arrived at `now`: how long they took, into the moving average.
    pub(crate) fn got(&mut self, frames: &[i64], now: f64) {
        for f in frames {
            let Some(at) = self.asked.remove(f) else { continue };
            let took = now - at;
            if !took.is_finite() || took < 0.0 {
                continue;
            }
            self.latency = Some(match self.latency {
                Some(old) => old + (took - old) * EMA,
                None => took,
            });
        }
    }

    /// How many comp frames ahead of the playhead the next frames are asked for, at `fps` comp
    /// frames a second: the playhead moves on while they render.
    pub(crate) fn lead(&self, fps: f64) -> i64 {
        lead_frames(self.latency.unwrap_or(FIRST_GUESS), fps)
    }

    /// The real-time readout (EffectCraft's Info panel's) and whether it is real time.
    pub(crate) fn rate(&self) -> Option<(String, bool)> {
        self.playback.rate_label()
    }
}

/// Comp frames the playhead moves on in `latency` seconds (asking for a frame to having it) at
/// `fps` comp frames a second, at most [`MAX_LEAD`] seconds' worth.
pub(crate) fn lead_frames(latency: f64, fps: f64) -> i64 {
    let secs = if latency.is_finite() { latency.clamp(0.0, MAX_LEAD) } else { 0.0 };
    let n = (secs * fps).ceil();
    if n.is_finite() { n.clamp(0.0, 1e6) as i64 } else { 0 }
}

/// The frames a playing card asks for next: on the preview's grid (`span`, comp seconds; `step`
/// 0 = every frame), from `lead` frames past the playhead frame `at` on, `n` at most, wrapping to
/// the range start when it loops; never a frame behind the playhead (it has been played).
pub(crate) fn order(c: &EcComp, span: &Span, at: i64, lead: i64, n: usize) -> Vec<i64> {
    let r = c.frames();
    let fps = if c.fps.is_finite() && c.fps > 0.0 { c.fps } else { 30.0 };
    let frame = |t: f64| -> i64 {
        let v = (t * fps).round();
        if v.is_finite() { v.clamp(-1e12, 1e12) as i64 } else { 0 }
    };
    let a = frame(span.start).max(*r.start());
    // `end` is exclusive (the card's stretch end): its last frame is one before.
    let b = (frame(span.end) - 1).min(*r.end());
    if b < a || n == 0 {
        return Vec::new();
    }
    let step = if span.step.is_finite() && span.step > 0.0 { frame(span.step).max(1) } else { 1 };
    // The grid frame at or after `f`.
    let on_grid = |f: i64| a + (f - a + step - 1).div_euclid(step) * step;
    let mut f = on_grid(at.clamp(a, b) + lead.max(0));
    let mut out = Vec::with_capacity(n);
    let mut wrapped = false;
    while out.len() < n {
        if f > b {
            if !span.looping || wrapped {
                break;
            }
            wrapped = true;
            f = a;
        }
        // Back round to where it started: every frame of the range is asked for.
        if wrapped && f >= at.clamp(a, b) {
            break;
        }
        out.push(f);
        f += step;
    }
    out
}

/// The frame a playing card shows for the playhead frame `at`, from the frames it holds
/// (`held`, ascending): `at` itself, else the newest one before it but not before the frame on
/// screen (`last`: frames only move forward while playing; after a loop round, `last` is ahead of
/// `at` and is ignored), else the frame on screen (held, not ready yet: it stays up). `None` only
/// before any frame of the card is held near the playhead, when the nearest one is used.
pub(crate) fn pick(held: &[i64], at: i64, last: Option<i64>) -> Option<i64> {
    let floor = last.filter(|l| *l <= at);
    let newest = held.iter().copied().filter(|f| *f <= at && floor.is_none_or(|l| *f >= l)).max();
    newest.or_else(|| last.filter(|l| held.contains(l)))
}

/// One meter, behind a cell so the card overlay (which draws from `&FigApp`) can note the frame
/// it shows.
pub(crate) type Shared = RefCell<Meter>;

#[cfg(test)]
#[path = "effectcraft_play_tests.rs"]
mod tests;
