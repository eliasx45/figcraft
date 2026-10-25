//! Audio layers on the canvas: the playhead and a play button over the strip, scrubbing along its
//! ruler, and the player that sounds what the engine's transport ([`figcraft_engine::audio`])
//! says is playing.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};
use figcraft_doc::{AudioClip, Document, NodeId};
use figcraft_geom::Point;
use serde_json::json;

use crate::FigApp;

/// Play button radius (screen points).
const BUTTON: f32 = 10.0;
/// The ruler band drawn at the top of a strip (layer units), as the renderer draws it.
const RULER: f64 = 16.0;
/// Sounds kept decoded (by hash) for playing again.
const KEEP_SOUNDS: usize = 8;

/// The player and the canvas's scrub state.
#[derive(Default)]
pub struct Player {
    /// The transport generation the output was started for.
    generation: u64,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    out: Option<out::Output>,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    sounds: HashMap<String, Arc<figcraft_audio::Sound>>,
    /// The layer whose ruler is being dragged.
    scrub: Option<NodeId>,
    /// The strip under the pointer when Space went down: it plays or pauses when Space is let go.
    pub(crate) space: Option<NodeId>,
    /// Why the last play made no sound (shown once).
    failed: Option<String>,
}

impl FigApp {
    /// Start, stop or restart the output when the transport changed; keep frames coming while it
    /// plays so the playhead moves.
    pub(crate) fn audio_sync(&mut self, ctx: &egui::Context) {
        let t = &self.session.audio;
        // The playhead's layer went away (deleted, undone, another file).
        if let Some(n) = t.node
            && !self.session.doc().is_ok_and(|d| d.node(n).is_some_and(|n| n.audio.is_some()))
        {
            self.session.audio.clear();
        }
        let t = &self.session.audio;
        if t.generation != self.audio.generation {
            self.audio.generation = t.generation;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                self.audio.out = None;
                if t.playing()
                    && let Err(e) = self.audio_start()
                    && self.audio.failed.as_deref() != Some(e.as_str())
                {
                    self.audio.failed = Some(e.clone());
                    self.toast(e);
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            if t.playing() && self.audio.failed.is_none() {
                let e = "Sound output isn't available on this platform yet: the playhead moves without sound".to_string();
                self.audio.failed = Some(e.clone());
                self.toast(e);
            }
        }
        if self.session.audio.playing() {
            ctx.request_repaint();
        } else {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                self.audio.out = None;
            }
        }
    }

    /// Open the playhead clip's sound (decoded once per file) and start the output at the playhead.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn audio_start(&mut self) -> Result<(), String> {
        let t = &self.session.audio;
        let (Some(node), from, until) = (t.node, t.time(), t.until()) else { return Ok(()) };
        let doc = self.session.doc().map_err(|e| e.to_string())?;
        let clip = doc.node(node).and_then(|n| n.audio.as_deref()).ok_or("not an audio layer")?;
        let sound = match self.audio.sounds.get(&clip.asset) {
            Some(s) => s.clone(),
            None => {
                let data = doc.audio.get(&clip.asset).ok_or("this sound's data is missing from the file: it can't play")?;
                let s = Arc::new(figcraft_audio::open(&clip.file_name, Arc::from(data.bytes.as_slice())).map_err(|e| e.to_string())?);
                if self.audio.sounds.len() >= KEEP_SOUNDS {
                    self.audio.sounds.clear();
                }
                self.audio.sounds.insert(clip.asset.clone(), s.clone());
                s
            }
        };
        self.audio.out = Some(out::Output::start(&sound, from, until)?);
        self.audio.failed = None;
        Ok(())
    }
}

/// The local point of page point `p` in layer `id`.
fn local(doc: &Document, id: NodeId, p: Point) -> Option<Point> {
    let w = doc.world_transform(id);
    (w.determinant().abs() > 1e-12).then(|| w.inverse() * p)
}

/// The topmost visible audio layer on `page` whose box holds page point `p`, however deeply it
/// sits in frames, groups or auto layout (hovering one shows its play button).
pub fn audio_at(doc: &Document, page: NodeId, p: Point) -> Option<NodeId> {
    let mut hit = None;
    let mut stack = vec![(page, 0usize)];
    // Children are pushed in reverse so the walk goes in paint order: the last hit is on top.
    while let Some((id, depth)) = stack.pop() {
        let Some(n) = doc.node(id) else { continue };
        if depth > 256 || !n.visible {
            continue;
        }
        if n.audio.is_some()
            && let Some(l) = local(doc, id, p)
            && (0.0..=n.width).contains(&l.x)
            && (0.0..=n.height).contains(&l.y)
        {
            hit = Some(id);
        }
        stack.extend(n.children.iter().rev().map(|c| (*c, depth + 1)));
    }
    hit
}

/// The audio layers overlays are drawn for: on the current page, selected, under the playhead or
/// under the pointer at `hover` (screen).
fn shown(app: &FigApp, hover: Option<Pos2>) -> Vec<NodeId> {
    let Ok(ds) = app.session.ds() else { return Vec::new() };
    let doc = &ds.doc;
    let mut ids: Vec<NodeId> = ds.selection.iter().copied().filter(|i| doc.node(*i).is_some_and(|n| n.audio.is_some())).collect();
    let hovered = hover.and_then(|h| {
        let p = app.to_page(h);
        audio_at(doc, ds.page, p)
    });
    for n in [app.session.audio.node, hovered].into_iter().flatten() {
        if !ids.contains(&n) && doc.page_of(n) == Some(ds.page) {
            ids.push(n);
        }
    }
    ids
}

/// The play button's centre on screen for layer `id` (inside its bottom-left corner), when the
/// strip shows big enough to hold it.
fn button_at(app: &FigApp, doc: &Document, id: NodeId) -> Option<Pos2> {
    let n = doc.node(id)?;
    let w = doc.world_transform(id);
    let (a, b) = (w * Point::new(0.0, n.height), w * Point::new(n.width, 0.0));
    let (sa, sb) = (app.to_screen(a.x, a.y), app.to_screen(b.x, b.y));
    let r = Rect::from_two_pos(sa, sb);
    (r.width() > BUTTON * 4.0 && r.height() > BUTTON * 3.0).then(|| pos2(r.min.x + BUTTON + 6.0, r.max.y - BUTTON - 6.0))
}

/// Draw the playhead and play buttons.
pub fn overlay(app: &FigApp, painter: &egui::Painter) {
    let Ok(ds) = app.session.ds() else { return };
    let doc = &ds.doc;
    let t = &app.session.audio;
    let hover = painter.ctx().input(|i| i.pointer.hover_pos()).filter(|h| painter.clip_rect().contains(*h));
    for id in shown(app, hover) {
        let Some(n) = doc.node(id) else { continue };
        let Some(a) = n.audio.as_deref() else { continue };
        let w = doc.world_transform(id);
        if t.node == Some(id) {
            let time = t.time();
            let x = a.x_of(time, n.width);
            if (0.0..=n.width).contains(&x) {
                let (p0, p1) = (w * Point::new(x, 0.0), w * Point::new(x, n.height));
                let (s0, s1) = (app.to_screen(p0.x, p0.y), app.to_screen(p1.x, p1.y));
                let col = Color32::from_rgb(0xFF, 0x4D, 0x4D);
                painter.line_segment([s0, s1], Stroke::new(2.0, col));
                painter.add(egui::Shape::convex_polygon(vec![s0 + vec2(-6.0, -8.0), s0 + vec2(6.0, -8.0), s0], col, Stroke::NONE));
                let label = format!("{}  {}", a.frame_of(time), timecode(time + a.offset));
                let font = egui::FontId::proportional(11.0);
                let g = painter.layout_no_wrap(label, font, Color32::WHITE);
                let r = Rect::from_min_size(s0 + vec2(4.0, -24.0), g.size() + vec2(8.0, 4.0));
                painter.rect_filled(r, CornerRadius::same(3), col);
                painter.galley(r.min + vec2(4.0, 2.0), g, Color32::WHITE);
            }
        }
        if let Some(c) = button_at(app, doc, id) {
            let playing = t.node == Some(id) && t.playing();
            painter.circle_filled(c, BUTTON, Color32::from_black_alpha(170));
            painter.circle_stroke(c, BUTTON, Stroke::new(1.0, Color32::from_white_alpha(90)));
            let fg = Color32::WHITE;
            if playing {
                for dx in [-3.5, 1.5] {
                    painter.rect_filled(Rect::from_min_size(c + vec2(dx, -4.5), vec2(2.5, 9.0)), CornerRadius::ZERO, fg);
                }
            } else {
                painter.add(egui::Shape::convex_polygon(vec![c + vec2(-3.0, -5.0), c + vec2(5.0, 0.0), c + vec2(-3.0, 5.0)], fg, Stroke::NONE));
            }
        }
    }
}

/// `m:ss.cc` of `t` seconds.
pub fn timecode(t: f64) -> String {
    if !t.is_finite() {
        return "–".into();
    }
    let sign = if t < 0.0 { "-" } else { "" };
    let cs = (t.abs() * 100.0).round() as i64;
    format!("{sign}{}:{:02}.{:02}", cs / 6000, (cs / 100) % 60, cs % 100)
}

/// A press on a play button plays or pauses; a press on a selected strip's ruler puts the
/// playhead there and drags it. Returns whether the pointer was used.
pub fn input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool) -> bool {
    let pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    let down = ctx.input(|i| i.pointer.primary_down());
    if let Some(id) = app.audio.scrub {
        if !down {
            app.audio.scrub = None;
        } else {
            seek_to(app, id, pos);
        }
        return true;
    }
    if !pressed || !hovered {
        return false;
    }
    let Ok(ds) = app.session.ds() else { return false };
    let doc = ds.doc.clone();
    let sel = ds.selection.clone();
    for id in shown(app, Some(pos)) {
        if button_at(app, &doc, id).is_some_and(|c| c.distance(pos) <= BUTTON + 2.0) {
            app.run("audio.togglePlay", json!({"id": id}));
            return true;
        }
    }
    let page = app.to_page(pos);
    for id in sel {
        let Some(n) = doc.node(id) else { continue };
        let Some(a) = n.audio.as_deref() else { continue };
        let Some(l) = local(&doc, id, page) else { continue };
        if a.ruler && (0.0..=n.width).contains(&l.x) && (0.0..=ruler(a, n.height)).contains(&l.y) {
            app.audio.scrub = Some(id);
            seek_to(app, id, pos);
            return true;
        }
    }
    false
}

/// Space tapped (pressed and let go with no click in between) with the pointer over an audio strip
/// plays or pauses it, no click needed; held while dragging, it still pans the canvas. `hover`: the
/// pointer on the canvas (screen); `free`: no text is being typed.
pub fn space(app: &mut FigApp, ctx: &egui::Context, hover: Option<Pos2>, free: bool) {
    let (down, up, click) = ctx.input(|i| {
        let (mut down, mut up) = (false, false);
        for e in &i.events {
            if let egui::Event::Key { key: egui::Key::Space, pressed, repeat, modifiers, .. } = e {
                down |= *pressed && !*repeat && modifiers.is_none();
                up |= !*pressed;
            }
        }
        (down, up, i.pointer.any_pressed())
    });
    if down && free {
        app.audio.space = hover.and_then(|h| {
            let ds = app.session.ds().ok()?;
            audio_at(&ds.doc, ds.page, app.to_page(h))
        });
    }
    if click {
        app.audio.space = None;
    }
    if up && let Some(id) = app.audio.space.take() {
        app.run("audio.togglePlay", json!({"id": id}));
    }
}

fn ruler(a: &AudioClip, h: f64) -> f64 {
    if a.ruler { RULER.min(h * 0.3) } else { 0.0 }
}

fn seek_to(app: &mut FigApp, id: NodeId, pos: Pos2) {
    let page = app.to_page(pos);
    let Some(x) = app.session.doc().ok().and_then(|d| local(d, id, page)).map(|l| l.x) else { return };
    app.run("audio.seek", json!({"id": id, "x": x}));
}

/// The sound output (macOS and Windows): the stretch from the playhead to the end of the clip's range,
/// decoded and resampled to the device rate up front, played from a shared buffer.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) mod out {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    pub struct Output {
        _stream: cpal::Stream,
    }

    impl Output {
        pub fn start(sound: &figcraft_audio::Sound, from: f64, until: f64) -> Result<Output, String> {
            let host = cpal::default_host();
            let device = host.default_output_device().ok_or("no sound output device")?;
            let cfg = device.default_output_config().map_err(|e| format!("sound output: {e}"))?;
            let rate = cfg.sample_rate().0;
            let channels = usize::from(cfg.channels().max(1));
            let pcm: Arc<Vec<f32>> = Arc::new(sound.stereo(from, until, rate));
            let config = cfg.config();
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::F32 => build::<f32>(&device, &config, pcm, channels),
                cpal::SampleFormat::I16 => build::<i16>(&device, &config, pcm, channels),
                cpal::SampleFormat::U16 => build::<u16>(&device, &config, pcm, channels),
                cpal::SampleFormat::I32 => build::<i32>(&device, &config, pcm, channels),
                f => return Err(format!("sound output: unsupported sample format {f}")),
            }?;
            stream.play().map_err(|e| format!("sound output: {e}"))?;
            Ok(Output { _stream: stream })
        }
    }

    fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        pcm: Arc<Vec<f32>>,
        channels: usize,
    ) -> Result<cpal::Stream, String> {
        let at = AtomicUsize::new(0);
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    let mut i = at.load(Ordering::Relaxed);
                    for frame in data.chunks_mut(channels) {
                        let (l, r) = (pcm.get(i * 2).copied().unwrap_or(0.0), pcm.get(i * 2 + 1).copied().unwrap_or(0.0));
                        for (c, s) in frame.iter_mut().enumerate() {
                            let v = match (channels, c) {
                                (1, _) => (l + r) * 0.5,
                                (_, 0) => l,
                                (_, 1) => r,
                                _ => 0.0,
                            };
                            *s = T::from_sample(v);
                        }
                        i += 1;
                    }
                    at.store(i, Ordering::Relaxed);
                },
                |e| log::warn!("sound output: {e}"),
                None,
            )
            .map_err(|e| format!("sound output: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::timecode;

    #[test]
    fn timecodes() {
        assert_eq!(timecode(42.743), "0:42.74");
        assert_eq!(timecode(61.005), "1:01.01");
        assert_eq!(timecode(-0.5), "-0:00.50");
        assert_eq!(timecode(f64::NAN), "–");
    }
}
