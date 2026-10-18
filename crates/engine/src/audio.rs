//! The audio transport: which clip plays, from where, and whether it is running. The clock is the
//! wall clock (the desktop app's player starts its output at the same moment), so agents read the
//! same playhead the user hears, also in headless sessions where nothing sounds.

use figcraft_doc::{AudioClip, NodeId};
use web_time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Transport {
    /// The audio layer the playhead is on.
    pub node: Option<NodeId>,
    /// Media seconds at `since` (or now, while paused).
    time: f64,
    /// When playback started; `None` while paused.
    since: Option<Instant>,
    /// Where playback stops (media seconds: the end of the clip's shown range).
    until: f64,
    /// Bumped on every play, pause and seek, so the player knows to restart its output.
    pub generation: u64,
}

impl Transport {
    pub fn playing(&self) -> bool {
        self.since.is_some() && self.time() < self.until
    }

    /// The playhead in media seconds.
    pub fn time(&self) -> f64 {
        match self.since {
            Some(t) => (self.time + t.elapsed().as_secs_f64()).min(self.until),
            None => self.time,
        }
    }

    /// Start playing `node`'s clip `a` at media second `from` (its range start when outside it).
    pub fn play(&mut self, node: NodeId, a: &AudioClip, from: Option<f64>) {
        let from = from.filter(|t| t.is_finite() && *t >= a.start && *t < a.end).unwrap_or(a.start);
        self.node = Some(node);
        self.time = from;
        self.until = a.end;
        self.since = Some(Instant::now());
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn pause(&mut self) {
        self.time = self.time();
        self.since = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Put the playhead on `node` at media second `t` (kept within the clip's range), playing on
    /// when it was playing.
    pub fn seek(&mut self, node: NodeId, a: &AudioClip, t: f64) {
        let t = if t.is_finite() { t.clamp(a.start, a.end) } else { a.start };
        let playing = self.playing() && self.node == Some(node);
        self.node = Some(node);
        self.time = t;
        self.until = a.end;
        self.since = playing.then(Instant::now);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Stop and forget the clip (its layer went away).
    pub fn clear(&mut self) {
        *self = Self { generation: self.generation.wrapping_add(1), ..Self::default() };
    }

    /// Where playback stops (media seconds).
    pub fn until(&self) -> f64 {
        self.until
    }
}
