//! Trackpad scrolling, reshaped for egui's wheel state (Ghostex, 29 Oct: "it scrolls in 2
//! movements instead of 1"). egui drops the delta a `Start` or `End` wheel event carries, and an
//! `End` clears every scroll its frame gathered; once ended, the momentum that follows a lifted
//! finger is smoothed (delayed over ~0.1 s) and its own `End` drops what was left. So: a `Start`
//! goes in without its delta and the delta follows as a `Move`, an `End`'s delta goes in as a
//! `Move` and the `End` itself waits for the next frame, and a `Start` coming before that frame
//! (the momentum after a lift) cancels the wait: one gesture, never smoothed.

use egui::{Event, TouchPhase, Vec2};

#[derive(Default)]
pub(crate) struct Wheel {
    /// A gesture's end waiting to go in, and whether a frame has run since it came.
    end: Option<(Event, bool)>,
}

impl Wheel {
    /// `event` as it should reach egui, pushed into `events` (the next frame's).
    pub(crate) fn push(&mut self, events: &mut Vec<Event>, event: Event) {
        let Event::MouseWheel { unit, delta, phase, modifiers } = event else {
            events.push(event);
            return;
        };
        let moved = |events: &mut Vec<Event>| {
            if delta != Vec2::ZERO {
                events.push(Event::MouseWheel { unit, delta, phase: TouchPhase::Move, modifiers });
            }
        };
        match phase {
            TouchPhase::Start => {
                if self.end.take().is_none() {
                    events.push(Event::MouseWheel { unit, delta: Vec2::ZERO, phase, modifiers });
                }
                moved(events);
            }
            TouchPhase::End | TouchPhase::Cancel => {
                moved(events);
                self.end = Some((Event::MouseWheel { unit, delta: Vec2::ZERO, phase, modifiers }, false));
            }
            TouchPhase::Move => events.push(event),
        }
    }

    /// Before a frame: the end a frame has already run since goes in first. Returns whether an
    /// end still waits (the host should run another frame for it).
    pub(crate) fn before_frame(&mut self, events: &mut Vec<Event>) -> bool {
        if self.end.as_ref().is_some_and(|(_, ripe)| *ripe)
            && let Some((end, _)) = self.end.take()
        {
            events.insert(0, end);
        }
        if let Some((_, ripe)) = &mut self.end {
            *ripe = true;
        }
        self.end.is_some()
    }
}
