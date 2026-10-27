//! Video cards on the canvas: a poster for every card on screen, the play button plays a card
//! with its sound on the video transport, "Play all visible" plays every card on screen (muted),
//! and a slider in the bar above the card scrubs. Like a native player: hovering alone never moves
//! the playhead; ⌘ (Ctrl elsewhere) held over the picture scrubs it under the pointer; Space plays
//! or pauses the card under the pointer (or the one being scrubbed, mid-drag too); ←/→ step a
//! frame while paused; M mutes; a speaker button and volume slider in the bar set the card's
//! volume live (the wheel over the slider too).
//!
//! Frames come from `figcraft-video` (the Apple media engine on macOS), decoded on worker threads
//! already scaled to the card: a player thread per playing card keeps a few frames ahead, one
//! worker makes posters and the stills a paused or scrubbed card shows. Cards off screen decode
//! nothing, so a page of hundreds of clips stays light.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};
use figcraft_doc::{Document, NodeId, VideoClip};
use serde_json::json;

use crate::FigApp;
use crate::effectcraft_ui::{CardBar, card_bar, chip, draw_track, fit, on_button, pointer, readout, round_button, screen_rect};

/// Button radius (screen points).
const BUTTON: f32 = 10.0;
/// Frame sizes decoded (longest side, pixels): the smallest at least as big as the card on screen.
const SIDES: [u32; 3] = [240, 480, 960];
/// Most cards playing at once, and frames each decodes ahead.
const MAX_PLAYERS: usize = 24;
const AHEAD: usize = 4;
/// Posters kept (the least recently shown go first) and asked for at once.
const MAX_POSTERS: usize = 400;
const POSTERS_OUT: usize = 24;
/// Biggest file whose sound is read for playback (bytes).
const MAX_SOUND_FILE: u64 = 1 << 30;
/// Width of a card's volume slider (screen points).
const VOLUME_W: f32 = 56.0;

/// A picture a card shows: the texture, the media time it is, and what it was made for.
struct Pic {
    tex: egui::TextureHandle,
    time: f64,
    side: u32,
}

/// A card playing: its decoding thread's frames, and the clock they're shown against.
struct Player {
    /// What it was started for: path, frame size, transport generation (`None`: its own clock),
    /// and how many times the clock wrapped.
    key: (String, u32, Option<u64>, u64),
    rx: Receiver<figcraft_video::Frame>,
    stop: Arc<AtomicBool>,
    /// The next frame, not due yet.
    next: Option<figcraft_video::Frame>,
    pic: Option<Pic>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A poster or still job for the worker, and what came back.
struct Job {
    node: NodeId,
    path: PathBuf,
    time: f64,
    side: u32,
    still: bool,
}

struct Done {
    node: NodeId,
    time: f64,
    side: u32,
    still: bool,
    result: Result<egui::TextureHandle, String>,
}

/// A file's sound, read off the UI thread (`None`: it has none, or it couldn't be read).
type Loaded = (String, Option<Arc<figcraft_audio::Sound>>);

enum SoundState {
    Loading,
    Ready(Arc<figcraft_audio::Sound>),
    Silent,
}

/// Video playback state.
#[derive(Default)]
pub struct Videos {
    tx: Option<Sender<Job>>,
    rx: Option<Receiver<Done>>,
    sound_rx: Option<(Sender<Loaded>, Receiver<Loaded>)>,
    posters: HashMap<NodeId, Pic>,
    /// When each card's poster was last on screen (eviction), and posters out at the worker.
    seen: HashMap<NodeId, f64>,
    asked: HashMap<NodeId, (u32, f64)>,
    /// Why a card's file can't play.
    failed: HashMap<NodeId, String>,
    /// The still a paused or scrubbed transport card shows, the last one asked for, and whether
    /// it is still out at the worker.
    still: Option<(NodeId, Pic)>,
    still_asked: Option<(NodeId, i64)>,
    still_out: bool,
    /// How many times the transport's card wrapped round its stretch (its player restarts).
    wraps: HashMap<NodeId, u64>,
    players: HashMap<NodeId, Player>,
    /// When "play all" started (each card's clock runs from then).
    all_since: f64,
    /// The card whose slider is being dragged.
    scrub: Option<NodeId>,
    /// The card ⌘ (Ctrl) scrubs: its playhead follows the pointer across the picture.
    cmd_scrub: Option<NodeId>,
    /// The card whose volume slider is being dragged.
    vol_drag: Option<NodeId>,
    /// Each card's volume (0–1, full when not set; this session) and its last level above zero
    /// (unmuting goes back to it).
    volume: HashMap<NodeId, f32>,
    unmuted: HashMap<NodeId, f32>,
    /// The playing sound's volume, shared with the sound output (an `f32`'s bits).
    gain: Arc<AtomicU32>,
    /// A video card took Space on its press: Space is the cards' until it is let go (no pan).
    space_owned: bool,
    /// The selected card Space plays or pauses when it is let go (a click while it is held pans
    /// instead).
    space: Option<NodeId>,
    sounds: HashMap<String, SoundState>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    out: Option<crate::audio_ui::out::Output>,
    out_for: Option<(u64, NodeId)>,
    last_time: f64,
    /// Whether the video, audio-strip and EffectCraft transports played last frame.
    was: (bool, bool, bool),
    /// The layers over each card, drawn over its picture ([`crate::card_cover`]).
    covers: std::cell::RefCell<crate::card_cover::Covers>,
}

impl Videos {
    /// Card `id`'s volume (0–1).
    pub(crate) fn volume_of(&self, id: NodeId) -> f32 {
        self.volume.get(&id).copied().filter(|l| l.is_finite()).map_or(1.0, |l| l.clamp(0.0, 1.0))
    }

    fn set_volume(&mut self, id: NodeId, level: f32) {
        let level = if level.is_finite() { level.clamp(0.0, 1.0) } else { 1.0 };
        if level > 0.0 {
            self.unmuted.insert(id, level);
        }
        self.volume.insert(id, level);
    }

    fn toggle_mute(&mut self, id: NodeId) {
        let level = self.volume_of(id);
        let next = toggle_mute(level, self.unmuted.get(&id).copied());
        self.set_volume(id, next);
    }

    /// Card `id`'s playhead is being dragged (its slider, or ⌘ over its picture).
    fn scrubbing(&self, id: NodeId) -> bool {
        self.scrub == Some(id) || self.cmd_scrub == Some(id)
    }

    /// A video card has Space (the canvas doesn't pan with it).
    pub(crate) fn owns_space(&self) -> bool {
        self.space_owned
    }
}

/// A video card's bar: [`card_bar`] with a speaker button and a volume slider taken from the right
/// end of its scrub track, when the file has sound and the track has room for both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VideoBar {
    pub bar: CardBar,
    pub track: Option<Rect>,
    pub speaker: Option<Pos2>,
    pub volume: Option<Rect>,
}

/// The bar of a video card whose picture shows in `pic` (`None` when it shows too small).
pub(crate) fn video_bar(pic: Rect, sound: bool) -> Option<VideoBar> {
    let bar = card_bar(pic)?;
    let mut out = VideoBar { bar, track: bar.track, speaker: None, volume: None };
    if let Some(t) = bar.track
        && sound
        && t.width() >= 40.0 + 8.0 + BUTTON * 2.0 + 4.0 + VOLUME_W
    {
        let volume = Rect::from_min_max(pos2(t.max.x - VOLUME_W, t.min.y), t.max);
        let speaker = pos2(volume.min.x - 4.0 - BUTTON, t.center().y);
        out.track = Some(Rect::from_min_max(t.min, pos2(speaker.x - BUTTON - 8.0, t.max.y)));
        out.speaker = Some(speaker);
        out.volume = Some(volume);
    }
    Some(out)
}

/// The media second under pointer `p` while ⌘ (Ctrl elsewhere) is held over the picture `pic`
/// with no button down; hovering alone never moves the playhead.
pub(crate) fn cmd_scrub_time(v: &VideoClip, pic: Rect, p: Pos2, command: bool, buttons_down: bool) -> Option<f64> {
    (command && !buttons_down && pic.width() > 0.0 && pic.contains(p)).then(|| slider_time(v, pic, p.x))
}

/// The volume (0–1) at screen x `x` on a volume slider `r`.
pub(crate) fn volume_at(r: Rect, x: f32) -> f32 {
    if r.width() > 0.0 && x.is_finite() { ((x - r.min.x) / r.width()).clamp(0.0, 1.0) } else { 1.0 }
}

/// M or the speaker button: mute, or back to the last level above zero (full when none).
pub(crate) fn toggle_mute(level: f32, last: Option<f32>) -> f32 {
    if level > 0.0 { 0.0 } else { last.filter(|l| l.is_finite() && *l > 0.0).map_or(1.0, |l| l.min(1.0)) }
}

/// The volume after a wheel scroll of `dy` points over the volume slider (up: louder).
pub(crate) fn scrolled_volume(level: f32, dy: f32) -> f32 {
    if dy.is_finite() { (level + dy * 0.004).clamp(0.0, 1.0) } else { level }
}

/// The media second one frame before (`dir` -1) or after (+1) media second `t`.
pub(crate) fn step_time(v: &VideoClip, t: f64, dir: i64) -> f64 {
    let f = v.frame_of(t).saturating_add(dir);
    let t = f as f64 / v.fps + 0.001 / v.fps;
    if t.is_finite() { t.max(v.start).min(v.end) } else { v.start }
}

/// The video cards on `page`, in paint order.
fn cards(doc: &Document, page: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![(page, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(n) = doc.node(id) else { continue };
        if depth > 256 || !n.visible {
            continue;
        }
        if n.video.is_some() {
            out.push(id);
        }
        stack.extend(n.children.iter().rev().map(|c| (*c, depth + 1)));
    }
    out
}

/// The frame size to decode for a card `px` pixels across on screen (at most the file's size).
pub(crate) fn side_for(px: f32, v: &VideoClip) -> u32 {
    let cap = v.width.max(v.height).max(SIDES[0]);
    let px = if px.is_finite() { px.max(0.0) } else { 0.0 };
    let fits = || SIDES.into_iter().filter(|s| *s <= cap);
    fits().find(|s| *s as f32 >= px).unwrap_or_else(|| fits().next_back().unwrap_or(SIDES[0]))
}

/// The media second at screen x `x` on a scrub track running along `track`.
pub(crate) fn slider_time(v: &VideoClip, track: Rect, x: f32) -> f64 {
    let u = if track.width() > 0.0 { f64::from(((x - track.min.x) / track.width()).clamp(0.0, 1.0)) } else { 0.0 };
    v.start + u * v.span()
}

/// A card's own clock `run` seconds after it started at `from`: the media second, and how many
/// times it wrapped round its stretch (it stops at the end when it doesn't loop).
pub(crate) fn clock(v: &VideoClip, from: f64, run: f64) -> (f64, u64) {
    let span = v.span();
    let t = from + run.max(0.0);
    if t < v.end || span <= 0.0 {
        (t, 0)
    } else if v.looping {
        let k = ((t - v.start) / span).floor().max(0.0);
        (v.start + (t - v.start) - k * span, k.min(1e9) as u64)
    } else {
        (v.end, 0)
    }
}

/// The poster's media second: a little way in (first frames are often black).
pub(crate) fn poster_time(v: &VideoClip) -> f64 {
    v.start + (v.span() * 0.1).min(1.0)
}

/// The media second card `id` shows now (the playing picture's, else the transport's, else the
/// poster's).
fn shown_time(app: &FigApp, id: NodeId, v: &VideoClip) -> f64 {
    if let Some(p) = app.videos.players.get(&id).and_then(|p| p.pic.as_ref()) {
        return p.time;
    }
    if app.session.video.node == Some(id) {
        return app.session.video.time();
    }
    poster_time(v)
}

/// Draw each card's picture, and on hover its buttons, label and slider. Runs right after the
/// canvas image, so selection outlines draw over it.
pub fn overlay(app: &FigApp, painter: &egui::Painter) {
    let Ok(ds) = app.session.ds() else { return };
    let doc = &ds.doc;
    let hover = pointer(painter);
    let tr = &app.session.video;
    let font = egui::FontId::proportional(11.0);
    let ids = cards(doc, ds.page);
    let mut covers = app.videos.covers.try_borrow_mut().ok();
    if let Some(cv) = covers.as_mut() {
        cv.keep_only(&ids);
    }
    let want = ds.view.zoom * f64::from(painter.ctx().pixels_per_point());
    for id in ids {
        let Some(v) = doc.node(id).and_then(|n| n.video.as_deref()) else { continue };
        let Some(r) = screen_rect(app, doc, id) else { continue };
        if !r.intersects(painter.clip_rect()) {
            continue;
        }
        let pic = fit(r, v.width, v.height);
        let still = app.videos.still.as_ref().filter(|(n, _)| *n == id).map(|(_, p)| p);
        let playing_pic = app.videos.players.get(&id).and_then(|p| p.pic.as_ref());
        let shown = playing_pic.or(still).or_else(|| app.videos.posters.get(&id));
        if let Some(p) = shown {
            painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgb(0x0C, 0x0C, 0x0E));
            painter.image(p.tex.id(), pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            // Layers above the card in z-order stay above its picture.
            if let Some(cv) = covers.as_mut() {
                cv.draw(painter, &ds.doc, id, want, |x, y| app.to_screen(x, y));
            }
        } else if let Some(why) = app.videos.failed.get(&id) {
            chip(
                painter,
                pos2(pic.min.x + 6.0, pic.max.y - 22.0),
                format!("{} · {why}", v.file_name()),
                font.clone(),
                Color32::from_black_alpha(190),
            );
        }
        // The controls sit in a strip above the card (`card_bar`), so nothing covers the picture.
        let vb = video_bar(pic, v.sound);
        let hovered = hover.is_some_and(|h| r.contains(h) || vb.is_some_and(|b| b.bar.zone.contains(h)));
        let playing = tr.playing() && tr.node == Some(id);
        let scrubbing = app.videos.scrubbing(id);
        let vol_drag = app.videos.vol_drag == Some(id);
        if !(hovered || playing || scrubbing || vol_drag) {
            continue;
        }
        let Some(vb) = vb else { continue };
        let bar = vb.bar;
        let t = shown_time(app, id, v);
        // The scrub track: where the picture is in the stretch.
        if let Some(track) = vb.track
            && v.span() > 0.0
        {
            let grip = app.videos.scrub == Some(id) || hover.is_some_and(|h| track.contains(h));
            draw_track(painter, track, None, Some(((t - v.start) / v.span()) as f32), grip, true);
        }
        // The speaker (mute) and the volume slider.
        if let (Some(sp), Some(vol)) = (vb.speaker, vb.volume) {
            let level = app.videos.volume_of(id);
            round_button(painter, sp);
            draw_speaker(painter, sp, level, Color32::WHITE);
            let grip = vol_drag || hover.is_some_and(|h| vol.contains(h));
            draw_track(painter, vol, None, Some(level), grip, true);
        }
        let sound = if playing && v.sound && vb.speaker.is_none() { "  🔊" } else { "" };
        readout(painter, bar.readout_right, bar.play.y, format!("{}{sound}", crate::audio_ui::timecode(t)), font.clone());
        {
            let (play, all) = (bar.play, bar.second);
            let fg = Color32::WHITE;
            round_button(painter, play);
            if playing {
                for dx in [-3.5, 1.5] {
                    painter.rect_filled(Rect::from_min_size(play + vec2(dx, -4.5), vec2(2.5, 9.0)), CornerRadius::ZERO, fg);
                }
            } else {
                painter.add(egui::Shape::convex_polygon(
                    vec![play + vec2(-3.0, -5.0), play + vec2(5.0, 0.0), play + vec2(-3.0, 5.0)],
                    fg,
                    Stroke::NONE,
                ));
            }
            if hovered {
                // Play all visible: two small triangles, lit while on.
                round_button(painter, all);
                if tr.play_all {
                    painter.circle_filled(all, BUTTON - 1.0, crate::theme::canvas_accent(painter.ctx()));
                }
                for dx in [-5.0, 0.5] {
                    let o = all + vec2(dx, 0.0);
                    painter.add(egui::Shape::convex_polygon(vec![o + vec2(0.0, -4.0), o + vec2(5.0, 0.0), o + vec2(0.0, 4.0)], fg, Stroke::NONE));
                }
            }
        }
    }
}

/// A speaker: its body and cone, then a cross when muted or one or two waves by level.
fn draw_speaker(painter: &egui::Painter, c: Pos2, level: f32, fg: Color32) {
    painter.rect_filled(Rect::from_min_size(c + vec2(-6.0, -2.5), vec2(3.0, 5.0)), CornerRadius::ZERO, fg);
    painter.add(egui::Shape::convex_polygon(
        vec![c + vec2(-3.0, -2.5), c + vec2(1.0, -5.5), c + vec2(1.0, 5.5), c + vec2(-3.0, 2.5)],
        fg,
        Stroke::NONE,
    ));
    let s = Stroke::new(1.3, fg);
    if level <= 0.0 {
        painter.line_segment([c + vec2(3.0, -3.0), c + vec2(7.0, 3.0)], s);
        painter.line_segment([c + vec2(3.0, 3.0), c + vec2(7.0, -3.0)], s);
        return;
    }
    let waves = if level > 0.5 { 2 } else { 1 };
    for k in 0..waves {
        let rad = 3.0 + 3.0 * k as f32;
        let pts: Vec<Pos2> = (0..=6).map(|i| (-0.9 + 0.3 * i as f32, rad)).map(|(a, rad)| c + vec2(1.0 + rad * a.cos(), rad * a.sin())).collect();
        painter.add(egui::Shape::line(pts, s));
    }
}

/// The topmost video card under screen point `p` (its card or its bar): the card, its clip, where
/// its picture shows and its bar.
fn card_under(app: &FigApp, p: Pos2) -> Option<(NodeId, VideoClip, Rect, Option<VideoBar>)> {
    let ds = app.session.ds().ok()?;
    let doc = &ds.doc;
    cards(doc, ds.page).into_iter().rev().find_map(|id| {
        let v = doc.node(id)?.video.as_deref()?;
        let r = screen_rect(app, doc, id)?;
        let pic = fit(r, v.width, v.height);
        let vb = video_bar(pic, v.sound);
        (r.contains(p) || vb.is_some_and(|b| b.bar.zone.contains(p))).then(|| (id, v.clone(), pic, vb))
    })
}

/// What the video cards took this frame, so the canvas doesn't also use it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Taken {
    /// Space (no Space-drag pan, no hand tool).
    pub space: bool,
    /// The wheel (it set a volume; the canvas doesn't scroll).
    pub scroll: bool,
}

/// A native player's keys and wheel on the card under the pointer (or the one being scrubbed):
/// Space plays or pauses at any time, mid-drag too (the drag goes on; a card playing while it is
/// scrubbed is silent and plays on from where it is let go); ⌘ (Ctrl) held scrubs under the
/// pointer; ←/→ step a frame while paused (not on a selected card: the arrows nudge it); M mutes
/// or unmutes; the wheel over the volume slider sets the volume. Away from the cards, Space taps
/// play the selected card as before. `pos`: the pointer on the canvas; `free`: no text is being
/// typed. Runs before the canvas's Space pan, scroll and the shortcuts.
pub fn keys(app: &mut FigApp, ctx: &egui::Context, pos: Option<Pos2>, free: bool) -> Taken {
    let under = pos.and_then(|p| card_under(app, p).map(|c| (p, c)));
    let (command, buttons, space_down, scroll) =
        ctx.input(|i| (i.modifiers.command, i.pointer.any_down(), i.key_down(egui::Key::Space), i.smooth_scroll_delta));
    // ⌘ (Ctrl) held over the picture: the playhead follows the pointer.
    let cmd = under.as_ref().and_then(|(p, (id, v, pic, _))| Some((*id, cmd_scrub_time(v, *pic, *p, command && free, buttons)?, v.fps)));
    app.videos.cmd_scrub = cmd.map(|c| c.0);
    if let Some((id, t, fps)) = cmd {
        let tr = &app.session.video;
        if tr.node != Some(id) || (tr.time() - t).abs() >= 0.5 / fps {
            app.run("video.seek", json!({"id": id, "time": t}));
        }
    }
    let focus = app.videos.scrub.or(app.videos.cmd_scrub).or(app.videos.vol_drag).or(under.as_ref().map(|(_, c)| c.0));
    // Space: the focused card plays or pauses on the press (not the release), held or not.
    let cmd_on = app.videos.cmd_scrub.is_some();
    let (down, up, click) = ctx.input(|i| {
        let (mut down, mut up) = (false, false);
        for e in &i.events {
            if let egui::Event::Key { key: egui::Key::Space, pressed, repeat, modifiers, .. } = e {
                let plain = modifiers.is_none() || (*modifiers == egui::Modifiers::COMMAND && cmd_on);
                down |= *pressed && !*repeat && plain;
                up |= !*pressed;
            }
        }
        (down, up, i.pointer.any_pressed())
    });
    if down
        && free
        && let Some(id) = focus
    {
        ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space);
        });
        app.videos.space = None;
        app.videos.space_owned = true;
        app.run("video.togglePlay", json!({"id": id}));
    } else if down && free {
        // Away from the cards: a tap plays the selected card; held while dragging, it pans.
        app.videos.space = selected_card(app);
    }
    if !space_down {
        app.videos.space_owned = false;
    }
    if click {
        app.videos.space = None;
    }
    if up && let Some(id) = app.videos.space.take() {
        app.run("video.togglePlay", json!({"id": id}));
    }
    let mut taken = Taken { space: app.videos.space_owned, scroll: false };
    let Some((p, (id, v, _, vb))) = under else { return taken };
    if !free {
        return taken;
    }
    // ←/→: a frame back or on while paused (a selected card's arrows nudge it, as before).
    let tr = &app.session.video;
    let paused = !(tr.playing() && tr.node == Some(id));
    if paused && !app.session.selection().contains(&id) {
        for (key, dir) in [(egui::Key::ArrowLeft, -1), (egui::Key::ArrowRight, 1)] {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                let t = step_time(&v, shown_time(app, id, &v), dir);
                app.run("video.seek", json!({"id": id, "time": t}));
            }
        }
    }
    if v.sound {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
            app.videos.toggle_mute(id);
        }
        // The wheel over the volume slider.
        if scroll.y != 0.0 && vb.and_then(|b| b.volume).is_some_and(|r| r.contains(p)) {
            let level = scrolled_volume(app.videos.volume_of(id), scroll.y);
            app.videos.set_volume(id, level);
            taken.scroll = true;
        }
    }
    taken
}

/// A press on a card's play or play-all button, its speaker (mute), its volume slider or its scrub
/// slider (both dragged). The card itself never moves or gets selected by these. Returns whether
/// the pointer was used.
pub fn input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool) -> bool {
    if let Some(id) = app.videos.vol_drag {
        if ctx.input(|i| i.pointer.primary_down()) {
            volume_to(app, id, pos);
        } else {
            app.videos.vol_drag = None;
        }
        return true;
    }
    if let Some(id) = app.videos.scrub {
        if ctx.input(|i| i.pointer.primary_down()) {
            scrub_to(app, id, pos);
        } else {
            app.videos.scrub = None;
        }
        return true;
    }
    if !hovered || !ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
        return false;
    }
    let Ok(ds) = app.session.ds() else { return false };
    let (doc, page) = (ds.doc.clone(), ds.page);
    // The controls are above the cards (`card_bar`): the topmost card whose strip is hit.
    for id in cards(&doc, page).into_iter().rev() {
        let Some(v) = doc.node(id).and_then(|n| n.video.as_deref()) else { continue };
        let Some(r) = screen_rect(app, &doc, id) else { continue };
        let Some(vb) = video_bar(fit(r, v.width, v.height), v.sound) else { continue };
        let bar = vb.bar;
        if on_button(bar.play, pos) {
            app.run("video.togglePlay", json!({"id": id}));
            return true;
        }
        if on_button(bar.second, pos) {
            app.run("video.playAll", json!({}));
            return true;
        }
        if vb.speaker.is_some_and(|s| on_button(s, pos)) {
            app.videos.toggle_mute(id);
            return true;
        }
        if vb.volume.is_some_and(|r| r.contains(pos)) {
            app.videos.vol_drag = Some(id);
            volume_to(app, id, pos);
            return true;
        }
        if vb.track.is_some_and(|t| t.contains(pos)) {
            app.videos.scrub = Some(id);
            scrub_to(app, id, pos);
            return true;
        }
    }
    false
}

/// The selected video card (the playing one when several are selected).
fn selected_card(app: &FigApp) -> Option<NodeId> {
    let doc = app.session.doc().ok()?;
    let has = |i: &NodeId| doc.node(*i).is_some_and(|n| n.video.is_some());
    let sel: Vec<NodeId> = app.session.selection().into_iter().filter(has).collect();
    sel.iter().copied().find(|i| app.session.video.node == Some(*i)).or_else(|| sel.first().copied())
}

fn scrub_to(app: &mut FigApp, id: NodeId, pos: Pos2) {
    let Ok(doc) = app.session.doc() else { return };
    let Some(v) = doc.node(id).and_then(|n| n.video.as_deref()) else { return };
    let Some(r) = screen_rect(app, doc, id) else { return };
    let Some(track) = video_bar(fit(r, v.width, v.height), v.sound).and_then(|b| b.track) else { return };
    let t = slider_time(v, track, pos.x);
    app.run("video.seek", json!({"id": id, "time": t}));
}

/// Set card `id`'s volume to the slider point under `pos`.
fn volume_to(app: &mut FigApp, id: NodeId, pos: Pos2) {
    let Ok(doc) = app.session.doc() else { return };
    let Some(v) = doc.node(id).and_then(|n| n.video.as_deref()) else { return };
    let Some(r) = screen_rect(app, doc, id) else { return };
    let Some(vol) = video_bar(fit(r, v.width, v.height), v.sound).and_then(|b| b.volume) else { return };
    let level = volume_at(vol, pos.x);
    app.videos.set_volume(id, level);
}

/// Decode frames of `path` from `from` until `until` at `side`, into `tx` (blocking while it is
/// `AHEAD` frames ahead), until `stop` or the end (a wrap starts a new decoder).
fn play_thread(path: PathBuf, from: f64, until: f64, side: u32, tx: SyncSender<figcraft_video::Frame>, stop: Arc<AtomicBool>) {
    let Ok(mut r) = figcraft_video::Reader::open(&path, from, until, side) else { return };
    while let Some(f) = r.next() {
        if stop.load(Ordering::Relaxed) || tx.send(f).is_err() {
            return;
        }
    }
}

fn texture(ctx: &egui::Context, name: String, f: &figcraft_video::Frame) -> Option<egui::TextureHandle> {
    let (w, h) = (f.width as usize, f.height as usize);
    if w == 0 || h == 0 || f.rgba.len() != w * h * 4 {
        return None;
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &f.rgba);
    Some(ctx.load_texture(name, img, egui::TextureOptions::LINEAR))
}

fn image(f: &figcraft_video::Frame) -> Option<egui::ColorImage> {
    let (w, h) = (f.width as usize, f.height as usize);
    (w > 0 && h > 0 && f.rgba.len() == w * h * 4).then(|| egui::ColorImage::from_rgba_unmultiplied([w, h], &f.rgba))
}

impl FigApp {
    /// Keep the video cards fed: posters for cards on screen, a player for each card that plays,
    /// the still a paused card shows, and the playing card's sound. Runs every frame after the UI.
    pub(crate) fn video_sync(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if let Some(n) = self.session.video.node
            && !self.session.doc().is_ok_and(|d| d.node(n).is_some_and(|n| n.video.is_some()))
        {
            self.session.video.clear();
        }
        self.video_one_sound();
        self.video_replies();
        let Ok(ds) = self.session.ds() else { return };
        let doc = ds.doc.clone();
        let ids = cards(&doc, ds.page);
        if ids.is_empty() {
            self.videos.players.clear();
            self.video_sound(None);
            return;
        }
        let ppp = ctx.pixels_per_point();
        // ⌘ (Ctrl) let go: the ⌘ scrub is over (it plays on from there when it was playing).
        if !ctx.input(|i| i.modifiers.command) {
            self.videos.cmd_scrub = None;
        }
        let tr = self.session.video.clone();
        let mut visible: Vec<(NodeId, VideoClip, u32, f32)> = Vec::new();
        for id in &ids {
            let Some(v) = doc.node(*id).and_then(|n| n.video.as_deref()) else { continue };
            let Some(r) = screen_rect(self, &doc, *id) else { continue };
            if !r.intersects(self.canvas.rect) {
                continue;
            }
            let pic = fit(r, v.width, v.height);
            visible.push((*id, v.clone(), side_for(pic.width().max(pic.height()) * ppp, v), pic.width() * pic.height()));
        }
        if tr.play_all && self.videos.all_since <= 0.0 {
            self.videos.all_since = now;
        } else if !tr.play_all {
            self.videos.all_since = 0.0;
        }
        // Who plays: the transport's card (unless scrubbed), and with "play all" every card on
        // screen, biggest first. Hovering plays nothing (⌘ scrubs).
        let mut want: Vec<(NodeId, VideoClip, u32, f64, Option<u64>)> = Vec::new();
        let by_size = {
            let mut v = visible.clone();
            v.sort_by(|a, b| b.3.total_cmp(&a.3));
            v
        };
        for (id, v, side, _) in &by_size {
            if self.videos.failed.contains_key(id) {
                continue;
            }
            let (from, run, generation) = if tr.playing() && tr.node == Some(*id) {
                if self.videos.scrubbing(*id) {
                    continue;
                }
                (tr.time(), 0.0, Some(tr.generation))
            } else if tr.play_all {
                (v.start, now - self.videos.all_since, None)
            } else {
                continue;
            };
            if want.len() >= MAX_PLAYERS {
                break;
            }
            want.push((*id, v.clone(), *side, from + run.max(0.0), generation));
        }
        let keep: Vec<NodeId> = want.iter().map(|w| w.0).collect();
        self.videos.players.retain(|id, _| keep.contains(id));
        self.videos.wraps.retain(|id, _| keep.contains(id));
        for (id, v, side, t, generation) in want {
            self.video_player(ctx, id, &v, side, t, generation);
        }
        // A paused (or scrubbed) transport card shows the frame at its playhead.
        match tr.node.filter(|n| !self.videos.players.contains_key(n)) {
            Some(n) => {
                if let Some((_, v, side, _)) = visible.iter().find(|(i, ..)| *i == n) {
                    let t = tr.time().min(v.end - 0.5 / v.fps).max(v.start);
                    let key = (n, (t * 1000.0).round() as i64);
                    // One still out at a time; the newest playhead is asked for once it's back.
                    if !self.videos.still_out && self.videos.still_asked != Some(key) {
                        self.videos.still_asked = Some(key);
                        self.videos.still_out = true;
                        self.video_job(Job { node: n, path: PathBuf::from(&v.path), time: t, side: *side, still: true });
                    }
                }
            }
            None => {
                self.videos.still = None;
                self.videos.still_asked = None;
            }
        }
        // Posters for cards on screen, at the size they show.
        for (id, v, side, _) in &visible {
            self.videos.seen.insert(*id, now);
            let have = self.videos.posters.get(id).is_some_and(|p| p.side >= *side);
            let out = self.videos.asked.get(id).is_some_and(|(s, _)| s >= side);
            if have || out || self.videos.failed.contains_key(id) || self.videos.asked.len() >= POSTERS_OUT {
                continue;
            }
            self.videos.asked.insert(*id, (*side, now));
            self.video_job(Job { node: *id, path: PathBuf::from(&v.path), time: poster_time(v), side: *side, still: false });
        }
        if self.videos.posters.len() > MAX_POSTERS {
            let mut by_seen: Vec<(NodeId, f64)> = self.videos.posters.keys().map(|k| (*k, self.videos.seen.get(k).copied().unwrap_or(0.0))).collect();
            by_seen.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in by_seen.into_iter().take(self.videos.posters.len() - MAX_POSTERS) {
                self.videos.posters.remove(&k);
            }
        }
        // Silent while it is scrubbed; it sounds again from where it is let go.
        let playing_card =
            tr.node.filter(|n| tr.playing() && !self.videos.scrubbing(*n)).and_then(|n| Some((n, doc.node(n)?.video.as_deref()?.clone())));
        self.video_sound(playing_card);
        if !self.videos.players.is_empty() || !self.videos.asked.is_empty() || self.videos.still_out {
            ctx.request_repaint();
        }
    }

    /// One sound at a time: a video card that starts playing pauses the audio strips and
    /// EffectCraft cards, and either of those starting pauses it.
    fn video_one_sound(&mut self) {
        let (vi, au, ec) = (self.session.video.playing(), self.session.audio.playing(), self.session.effectcraft.playing());
        if vi && (au || ec) {
            if !self.videos.was.0 {
                self.session.audio.pause();
                self.session.effectcraft.pause();
            } else {
                self.session.video.pause();
            }
        }
        self.videos.was = (self.session.video.playing(), self.session.audio.playing(), self.session.effectcraft.playing());
    }

    fn video_job(&mut self, job: Job) {
        if self.videos.tx.is_none() {
            let (jtx, jrx) = channel::<Job>();
            let (dtx, drx) = channel::<Done>();
            let Some(ctx) = self.ui_context.clone() else { return };
            let started = std::thread::Builder::new().name("video-stills".into()).spawn(move || {
                while let Ok(j) = jrx.recv() {
                    let r = figcraft_video::frame_at(&j.path, j.time, j.side).map_err(|e| e.to_string());
                    let result = r.and_then(|f| texture(&ctx, format!("video-{}-{}", j.node, j.still), &f).ok_or_else(|| "a bad frame".to_string()));
                    if dtx.send(Done { node: j.node, time: j.time, side: j.side, still: j.still, result }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            if started.is_err() {
                return;
            }
            (self.videos.tx, self.videos.rx) = (Some(jtx), Some(drx));
        }
        if let Some(tx) = &self.videos.tx
            && tx.send(job).is_err()
        {
            self.videos.tx = None;
            self.videos.rx = None;
        }
    }

    fn video_replies(&mut self) {
        let Some(rx) = &self.videos.rx else { return };
        let done: Vec<Done> = rx.try_iter().collect();
        for d in done {
            if d.still {
                self.videos.still_out = false;
            } else {
                self.videos.asked.remove(&d.node);
            }
            match d.result {
                Ok(tex) => {
                    let pic = Pic { tex, time: d.time, side: d.side };
                    if d.still {
                        self.videos.still = Some((d.node, pic));
                    } else {
                        self.videos.failed.remove(&d.node);
                        self.videos.posters.insert(d.node, pic);
                    }
                }
                Err(e) => {
                    if !d.still {
                        self.videos.failed.insert(d.node, e);
                    }
                }
            }
        }
    }

    /// Keep card `id` playing at media second `t` (its playhead now): start (or restart) its
    /// decoder when what it plays changed, and show the frame that is due.
    fn video_player(&mut self, ctx: &egui::Context, id: NodeId, v: &VideoClip, side: u32, t: f64, generation: Option<u64>) {
        let (t, wraps) = match generation {
            // The transport wraps on its own: a picture ahead of its playhead means it did.
            Some(_) => {
                let ahead = self.videos.players.get(&id).and_then(|p| p.pic.as_ref()).is_some_and(|p| p.time > t + 0.25);
                let w = self.videos.wraps.entry(id).or_insert(0);
                if ahead {
                    *w += 1;
                }
                (t, *w)
            }
            // On its own clock the card plays from its start; each wrap starts it again.
            None => clock(v, v.start, t - v.start),
        };
        let key = (v.path.clone(), side, generation, wraps);
        if self.videos.players.get(&id).is_none_or(|p| p.key != key) {
            let (tx, rx) = sync_channel::<figcraft_video::Frame>(AHEAD);
            let stop = Arc::new(AtomicBool::new(false));
            let (path, end, s2) = (PathBuf::from(&v.path), v.end, stop.clone());
            let spawned = std::thread::Builder::new().name("video-play".into()).spawn(move || play_thread(path, t, end, side, tx, s2));
            if spawned.is_err() {
                return;
            }
            // Keep showing the last picture until the new decoder's first frame (as of now).
            let pic = self.videos.players.remove(&id).and_then(|mut p| p.pic.take()).map(|mut p| {
                p.time = t;
                p
            });
            self.videos.players.insert(id, Player { key, rx, stop, next: None, pic });
        }
        let Some(p) = self.videos.players.get_mut(&id) else { return };
        let due = t + 0.5 / v.fps;
        loop {
            let f = match p.next.take() {
                Some(f) => f,
                None => match p.rx.try_recv() {
                    Ok(f) => f,
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                },
            };
            if f.time > due {
                p.next = Some(f);
                break;
            }
            // Due: show it (a later due frame replaces it in this same loop).
            match (&mut p.pic, image(&f)) {
                (Some(pic), Some(img)) if pic.side == side => {
                    pic.tex.set(img, egui::TextureOptions::LINEAR);
                    pic.time = f.time;
                }
                _ => {
                    if let Some(tex) = texture(ctx, format!("video-play-{id}"), &f) {
                        p.pic = Some(Pic { tex, time: f.time, side });
                    }
                }
            }
        }
    }

    /// The playing card's sound (when it has sound): the file's sound track, read once, played
    /// from the playhead; restarted on every play, seek and wrap.
    fn video_sound(&mut self, card: Option<(NodeId, VideoClip)>) {
        let loaded: Vec<Loaded> = self.videos.sound_rx.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
        for (path, sound) in loaded {
            self.videos.sounds.insert(path, sound.map_or(SoundState::Silent, SoundState::Ready));
        }
        let t = self.session.video.time();
        let wrapped = t + 0.05 < self.videos.last_time;
        self.videos.last_time = t;
        let Some((node, v)) = card.filter(|(_, v)| v.sound) else {
            self.videos.out_for = None;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                self.videos.out = None;
            }
            return;
        };
        self.videos.gain.store(self.videos.volume_of(node).to_bits(), Ordering::Relaxed);
        let want = (self.session.video.generation, node);
        if self.videos.out_for == Some(want) && !wrapped {
            return;
        }
        match self.videos.sounds.get(&v.path) {
            Some(SoundState::Ready(sound)) => {
                let sound = sound.clone();
                self.videos.out_for = Some(want);
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    self.videos.out = None;
                    match crate::audio_ui::out::Output::start_with_gain(&sound, t.max(0.0), v.end, self.videos.gain.clone()) {
                        Ok(o) => self.videos.out = Some(o),
                        Err(e) => log::warn!("video sound: {e}"),
                    }
                }
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let _ = sound;
            }
            Some(SoundState::Loading | SoundState::Silent) => {}
            None => {
                if self.videos.sounds.len() >= 4 {
                    self.videos.sounds.retain(|_, s| matches!(s, SoundState::Loading));
                }
                self.videos.sounds.insert(v.path.clone(), SoundState::Loading);
                let tx = match &self.videos.sound_rx {
                    Some((tx, _)) => tx.clone(),
                    None => {
                        let (tx, rx) = channel();
                        self.videos.sound_rx = Some((tx.clone(), rx));
                        tx
                    }
                };
                let (path, ctx) = (v.path.clone(), self.ui_context.clone());
                let _ = std::thread::Builder::new().name("video-sound".into()).spawn(move || {
                    let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_SOUND_FILE);
                    let sound = small
                        .then(|| std::fs::read(&path).ok())
                        .flatten()
                        .and_then(|bytes| figcraft_audio::open(&path, Arc::from(bytes.as_slice())).ok())
                        .map(Arc::new);
                    let _ = tx.send((path, sound));
                    if let Some(ctx) = ctx {
                        ctx.request_repaint();
                    }
                });
            }
        }
    }
}
