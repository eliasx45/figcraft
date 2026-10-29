//! The EffectCraft card transport: which card plays, from where, looping over its stretch. Like
//! the audio transport ([`crate::audio`]) it runs on the wall clock, so agents read the same
//! playhead the user sees, also in headless sessions where nothing draws.
//!
//! A card plays by EffectCraft's own preview rules when the app knows them ([`Preview`], set per
//! card with [`Transport::set_preview`]: the comp's work area and the Space preview options of its
//! project): it loops (or plays once) over the preview range (the work area, extended by the
//! current time, the whole comp or around the current time) within the card's stretch, starting
//! at the current time when that is inside the range (else at its start, or always at its start),
//! at the preview frame rate, showing every (skip + 1)-th frame. Without rules it loops over the
//! card's stretch as before.

use std::collections::HashMap;

use figcraft_doc::{EcComp, NodeId};
use web_time::Instant;

/// Which stretch a preview plays (EffectCraft's Preview panel ▸ Range).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PreviewRange {
    /// The work area.
    WorkArea,
    /// The work area, grown to take in the current time.
    #[default]
    WorkAreaExtended,
    /// The whole comp.
    Entire,
    /// `pre` seconds before to `post` seconds after the current time.
    Around { pre: f64, post: f64 },
}

/// EffectCraft's preview rules for one card's comp (comp seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    /// The comp's work area and duration.
    pub work: (f64, f64),
    pub duration: f64,
    pub range: PreviewRange,
    /// Loop (else play once and stop at the end).
    pub looping: bool,
    /// Always start at the range start (else at the current time when it is inside the range).
    pub from_start: bool,
    /// Comp frames shown per second (`None`: the comp's frame rate).
    pub fps: Option<f64>,
    /// Frames skipped between the ones shown.
    pub skip: u32,
}

impl Default for Preview {
    fn default() -> Self {
        Preview {
            work: (0.0, f64::INFINITY),
            duration: f64::INFINITY,
            range: PreviewRange::default(),
            looping: true,
            from_start: false,
            fps: None,
            skip: 0,
        }
    }
}

/// The stretch a card plays (comp seconds) when play is pressed at comp second `now`: the preview
/// range `p` makes of `now`, within the card's stretch `card`; the card's stretch when that is
/// empty or there are no rules.
pub fn play_range(card: (f64, f64), p: Option<&Preview>, now: f64) -> (f64, f64) {
    let Some(p) = p else { return card };
    let finite = |x: f64, or: f64| if x.is_finite() { x } else { or };
    let dur = finite(p.duration, card.1).max(0.0);
    let (wa, wb) = (finite(p.work.0, 0.0).max(0.0), finite(p.work.1, dur).min(dur));
    let (wa, wb) = (wa, wb.max(wa));
    let now = if now.is_finite() { now } else { wa };
    let (a, b) = match p.range {
        PreviewRange::WorkArea => (wa, wb),
        PreviewRange::WorkAreaExtended => (wa.min(now), wb.max(now)),
        PreviewRange::Entire => (0.0, dur),
        PreviewRange::Around { pre, post } => ((now - finite(pre, 0.0).max(0.0)).max(0.0), (now + finite(post, 0.0).max(0.0)).min(dur)),
    };
    let (a, b) = (a.max(card.0), b.min(card.1));
    if b > a { (a, b) } else { card }
}

/// Where a card starts playing in `range` when play is pressed at comp second `now`.
pub fn play_from(range: (f64, f64), p: Option<&Preview>, now: Option<f64>) -> f64 {
    let from_start = p.is_some_and(|p| p.from_start);
    match now.filter(|t| t.is_finite() && *t >= range.0 && *t < range.1) {
        Some(t) if !from_start => t,
        _ => range.0,
    }
}

/// The playhead `run` comp seconds after the start of play (`run` counted from the range start's
/// time base) inside `range`: wrapped round it when looping, held on its end when not (`None`:
/// played out).
pub fn wrap(run: f64, range: (f64, f64), looping: bool) -> Option<f64> {
    let (a, b) = range;
    let span = b - a;
    if span.is_nan() || span <= 0.0 || run < b {
        return Some(run);
    }
    if looping { Some(a + (run - a).rem_euclid(span)) } else { None }
}

/// What a playing card plays ([`Transport::span`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    /// The frame grid shown (seconds; Skip shows every n-th frame).
    pub step: f64,
    pub looping: bool,
    /// Comp seconds per second of playback.
    pub speed: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Transport {
    /// The card the playhead is on.
    pub node: Option<NodeId>,
    /// Comp seconds at `since` (or now, while paused).
    time: f64,
    /// When playback started; `None` while paused.
    since: Option<Instant>,
    /// The stretch playback loops over (comp seconds): the preview range within the card's.
    start: f64,
    end: f64,
    /// Loop (else stop at `end`), comp seconds per second, and the frame grid (seconds) shown.
    looping: bool,
    speed: f64,
    step: f64,
    /// The card's stretch, and the comp second play was pressed at (a preview range that follows
    /// the current time is taken from there when the rules change mid-play).
    card: (f64, f64),
    origin: f64,
    /// EffectCraft's preview rules per card ([`Transport::set_preview`]).
    previews: HashMap<NodeId, Preview>,
    /// Bumped on every play, pause and seek, so the player knows to restart its sound.
    pub generation: u64,
}

impl Transport {
    /// Playing (a card that plays once stops at its range end).
    pub fn playing(&self) -> bool {
        self.since.is_some() && self.raw().1
    }

    /// The comp second played to now, and whether it is still playing.
    fn raw(&self) -> (f64, bool) {
        let Some(t) = self.since else { return (self.time, false) };
        let speed = if self.speed.is_finite() && self.speed > 0.0 { self.speed } else { 1.0 };
        let run = self.time + t.elapsed().as_secs_f64() * speed;
        match wrap(run, (self.start, self.end), self.looping) {
            Some(x) => (x, true),
            // Played once to the end: the last frame stays.
            None => ((self.end - self.step.max(1e-6) * 0.5).max(self.start), false),
        }
    }

    /// The playhead in comp seconds (wrapping round the preview range while playing, on the
    /// preview's frame grid).
    pub fn time(&self) -> f64 {
        let (t, playing) = self.raw();
        if playing && self.since.is_some() && self.step > 0.0 && self.step.is_finite() {
            // Skipped frames: the last shown one.
            let n = ((t - self.start) / self.step + 1e-9).floor();
            return (self.start + n * self.step).max(self.start);
        }
        t
    }

    /// While playing: what is played (comp seconds `start..end`, every `step` seconds, looping or
    /// not, `speed` comp seconds per second), for the frames fetched ahead of the playhead.
    pub fn span(&self) -> Option<Span> {
        self.playing().then_some(Span { start: self.start, end: self.end, step: self.step, looping: self.looping, speed: self.speed })
    }

    /// EffectCraft's preview rules for card `node` (`None`: loop over the card's stretch). While
    /// that card plays, the new rules take effect at once (a changed work area is the next loop's).
    pub fn set_preview(&mut self, node: NodeId, p: Option<Preview>) {
        if self.previews.get(&node) == p.as_ref() {
            return;
        }
        match p {
            Some(p) => self.previews.insert(node, p),
            None => self.previews.remove(&node),
        };
        if self.node == Some(node) && self.since.is_some() {
            let now = self.time();
            let pv = self.previews.get(&node).copied();
            (self.start, self.end) = play_range(self.card, pv.as_ref(), self.origin);
            self.apply(pv.as_ref(), None);
            // Carry on from where it is (into the new range when it was left out).
            self.time = if now >= self.start && now < self.end { now } else { self.start };
            self.since = Some(Instant::now());
        }
    }

    /// The rules for card `node`.
    pub fn preview(&self, node: NodeId) -> Option<&Preview> {
        self.previews.get(&node)
    }

    fn apply(&mut self, p: Option<&Preview>, c: Option<&EcComp>) {
        self.looping = p.is_none_or(|p| p.looping);
        let comp_fps = c.map(|c| c.fps).filter(|f| f.is_finite() && *f > 0.0);
        let fps = p.and_then(|p| p.fps).filter(|f| f.is_finite() && *f > 0.0);
        if let Some(c) = comp_fps {
            self.speed = fps.map_or(1.0, |f| f / c);
            let skip = p.map_or(0, |p| p.skip.min(99));
            self.step = if skip > 0 { f64::from(skip + 1) / c } else { 0.0 };
        }
    }

    /// Start playing `node`'s card at comp second `from` (by its preview rules: the range start
    /// when `from` is outside the preview range, or always when they say so).
    pub fn play(&mut self, node: NodeId, c: &EcComp, from: Option<f64>) {
        let p = self.previews.get(&node).copied();
        let now = from.filter(|t| t.is_finite()).unwrap_or(c.start);
        self.card = (c.start, c.end);
        (self.start, self.end) = play_range(self.card, p.as_ref(), now);
        self.origin = now;
        self.speed = 1.0;
        self.step = 0.0;
        self.apply(p.as_ref(), Some(c));
        self.node = Some(node);
        self.time = play_from((self.start, self.end), p.as_ref(), from);
        self.since = Some(Instant::now());
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn pause(&mut self) {
        self.time = self.time();
        self.since = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Put the playhead on `node` at comp second `t` (kept within its stretch), playing on when
    /// it was playing.
    pub fn seek(&mut self, node: NodeId, c: &EcComp, t: f64) {
        let last = (c.end - 0.5 / c.fps).max(c.start);
        let t = if t.is_finite() { t.clamp(c.start, last) } else { c.start };
        let playing = self.playing() && self.node == Some(node);
        self.node = Some(node);
        self.time = t;
        if !playing {
            let p = self.previews.get(&node).copied();
            self.card = (c.start, c.end);
            (self.start, self.end) = play_range(self.card, p.as_ref(), t);
            self.origin = t;
            self.apply(p.as_ref(), Some(c));
        }
        self.since = playing.then(Instant::now);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Stop and forget the card (its layer went away).
    pub fn clear(&mut self) {
        let previews = std::mem::take(&mut self.previews);
        *self = Self { generation: self.generation.wrapping_add(1), previews, ..Self::default() };
    }
}
