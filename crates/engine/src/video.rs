//! The video card transport: which card plays (with its sound), from where, looping over its
//! stretch, and whether every card on screen plays (muted). Like the EffectCraft transport
//! ([`crate::effectcraft`]) it runs on the wall clock, so agents read the playhead the user sees.

use figcraft_doc::{NodeId, VideoClip};
use web_time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Transport {
    /// The card the playhead is on.
    pub node: Option<NodeId>,
    /// Media seconds at `since` (or now, while paused).
    time: f64,
    /// When playback started; `None` while paused.
    since: Option<Instant>,
    /// The card's stretch (media seconds) and whether it loops.
    start: f64,
    end: f64,
    looping: bool,
    /// Bumped on every play, pause and seek, so the player knows to restart.
    pub generation: u64,
    /// Every card on screen plays, muted ("Play all visible").
    pub play_all: bool,
}

impl Transport {
    pub fn playing(&self) -> bool {
        self.since.is_some()
    }

    /// The playhead in media seconds (wrapping round the stretch while a looping card plays; a
    /// card that doesn't loop stops at its end).
    pub fn time(&self) -> f64 {
        match self.since {
            Some(t) => {
                let span = self.end - self.start;
                let run = self.time + t.elapsed().as_secs_f64();
                if run < self.end || span <= 0.0 {
                    run
                } else if self.looping {
                    self.start + (run - self.start).rem_euclid(span)
                } else {
                    self.end
                }
            }
            None => self.time,
        }
    }

    /// Start playing `node`'s card at media second `from` (its start when outside its stretch).
    pub fn play(&mut self, node: NodeId, v: &VideoClip, from: Option<f64>) {
        let from = from.filter(|t| t.is_finite() && *t >= v.start && *t < v.end).unwrap_or(v.start);
        self.node = Some(node);
        self.time = from;
        (self.start, self.end, self.looping) = (v.start, v.end, v.looping);
        self.since = Some(Instant::now());
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn pause(&mut self) {
        self.time = self.time();
        self.since = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Put the playhead on `node` at media second `t` (kept within its stretch), playing on when it
    /// was playing.
    pub fn seek(&mut self, node: NodeId, v: &VideoClip, t: f64) {
        let last = (v.end - 0.5 / v.fps).max(v.start);
        let t = if t.is_finite() { t.clamp(v.start, last) } else { v.start };
        let playing = self.playing() && self.node == Some(node);
        self.node = Some(node);
        self.time = t;
        (self.start, self.end, self.looping) = (v.start, v.end, v.looping);
        self.since = playing.then(Instant::now);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Stop and forget the card (its layer went away); "play all" stays as it was.
    pub fn clear(&mut self) {
        *self = Self { generation: self.generation.wrapping_add(1), play_all: self.play_all, ..Self::default() };
    }
}
