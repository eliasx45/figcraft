//! The EffectCraft card transport: which card plays, from where, looping over its stretch. Like
//! the audio transport ([`crate::audio`]) it runs on the wall clock, so agents read the same
//! playhead the user sees, also in headless sessions where nothing draws.

use figcraft_doc::{EcComp, NodeId};
use web_time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Transport {
    /// The card the playhead is on.
    pub node: Option<NodeId>,
    /// Comp seconds at `since` (or now, while paused).
    time: f64,
    /// When playback started; `None` while paused.
    since: Option<Instant>,
    /// The card's stretch (comp seconds): playback loops over it.
    start: f64,
    end: f64,
    /// Bumped on every play, pause and seek, so the player knows to restart its sound.
    pub generation: u64,
}

impl Transport {
    pub fn playing(&self) -> bool {
        self.since.is_some()
    }

    /// The playhead in comp seconds (wrapping round the card's stretch while playing).
    pub fn time(&self) -> f64 {
        match self.since {
            Some(t) => {
                let span = self.end - self.start;
                let run = self.time + t.elapsed().as_secs_f64();
                if span > 0.0 && run >= self.end { self.start + (run - self.start).rem_euclid(span) } else { run }
            }
            None => self.time,
        }
    }

    /// Start playing `node`'s card at comp second `from` (its start when outside its stretch).
    pub fn play(&mut self, node: NodeId, c: &EcComp, from: Option<f64>) {
        let from = from.filter(|t| t.is_finite() && *t >= c.start && *t < c.end).unwrap_or(c.start);
        self.node = Some(node);
        self.time = from;
        (self.start, self.end) = (c.start, c.end);
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
        (self.start, self.end) = (c.start, c.end);
        self.since = playing.then(Instant::now);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Stop and forget the card (its layer went away).
    pub fn clear(&mut self) {
        *self = Self { generation: self.generation.wrapping_add(1), ..Self::default() };
    }
}
