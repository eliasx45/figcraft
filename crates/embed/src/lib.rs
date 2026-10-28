//! Fork-only: FigCraft's canvas inside another app's native view, in the same process (Ghostex's
//! EDITING tab, 28 Oct 2026, Elias: "perfect i love it as a start. go ahead").
//!
//! Ghostex shows a board of live EffectCraft cards (one per scene comp of MAIN, one row per
//! chapter) where its embedded EffectCraft viewer was. Ghostex adds a child `NSView` with a
//! `CAMetalLayer`, makes a [`wgpu::Surface`] for it (the one `unsafe` step, on the host's side)
//! and hands it here with the view's input. This crate does what eframe does for the desktop app,
//! without winit's event loop (the host owns the run loop), modelled on EffectCraft's
//! `effectcraft-embed`:
//!
//! - builds the [`FigApp`] as `apps/figcraft` does (an empty session, the person's FigCraft
//!   preferences read but never written, file dialogs, the clipboard), with EffectCraft cards
//!   in-process (`effectcraft-inproc`) on this view's wgpu device;
//! - opens the project's board ([`FigEmbedded::open`]): `<project stem>.board.figcraft` next to
//!   the `.ecproj`, made with `board.autoMain` when there is none yet, auto-saved;
//! - runs one egui pass per [`FigEmbedded::run_frame`] and paints it with egui-wgpu into the
//!   host's surface; only the canvas shows (the tab strip and the left rail sit outside the
//!   drawable, the side panels are folded), so the host draws its own toolbar around it;
//! - runs FigCraft commands in process ([`FigEmbedded::execute`]), hands over review marks
//!   ([`FigEmbedded::new_annotations`]) and tells the host what was selected or clicked
//!   ([`FrameOutput::events`]);
//! - optionally serves FigCraft's control channel (the desktop app's own server, compiled here)
//!   so agents can drive and screenshot the canvas.
//!
//! The board never writes the `.ecproj`: EffectCraft cards save through EffectCraft (the app the
//! card mirrors, or the Timeline's ⌘S).
//!
//! One per thread (FigCraft keeps its app in a thread-local, [`figcraft_ui_egui::windows`]), used
//! from the host's main thread. No `unsafe` here.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod board;
mod gpu;
mod services;

/// AppKit keys → egui events: EffectCraft's embed's mapping (AZERTY letters by the layout,
/// ⌘C/⌘X/⌘V as Copy/Cut/Paste), shared so both embedded views read keys the same way.
#[allow(dead_code)]
#[rustfmt::skip] // EffectCraft's file, in EffectCraft's format.
#[path = "../../../../effectcraft/crates/embed/src/keys.rs"]
mod keys;

// The desktop app's control server (it doesn't touch the window), compiled here as well.
#[allow(dead_code)]
#[path = "../../../apps/figcraft/src/control_server.rs"]
mod control_server;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use board::{BOARD_EXT, CardRef, NodeRef, board_path};
pub use eframe::{egui_wgpu, wgpu};
pub use egui;
pub use keys::{MacKey, mac_modifiers};

use egui_wgpu::capture::{CaptureReceiver, CaptureSender, CaptureState, capture_channel};
use figcraft_ui_egui::windows::with_app;
use figcraft_ui_egui::{ControlRequest, FigApp};
use serde_json::{Value, json};

/// What the host asks for.
#[derive(Clone, Debug, Default)]
pub struct EmbedOptions {
    /// Serve FigCraft's control channel on `127.0.0.1:<port>` (`None`: off).
    pub control_port: Option<u16>,
    /// The EffectCraft control port cards mirror (the host's in-process EffectCraft). FigCraft's
    /// cards read `EFFECTCRAFT_CONTROL_PORT` (else 9877); until FigCraft takes the port from its
    /// host this is only checked against it and a mismatch logged.
    pub effectcraft_port: Option<u16>,
    /// Show FigCraft's own chrome (tab strip, rail, panels) instead of the bare canvas.
    pub chrome: bool,
}

/// What happened on the canvas that the host follows (its scene list and overview).
#[derive(Clone, Debug, PartialEq)]
pub enum FigEvent {
    /// The selection changed: the cards in it (a layer inside a card counts as its card) and every
    /// selected node.
    Selection { cards: Vec<CardRef>, nodes: Vec<NodeRef> },
    /// A click (press and release without a drag) on the canvas: the card under it, else the node.
    Clicked { card: Option<CardRef>, node: Option<NodeRef> },
}

/// What one frame asks of the host.
#[derive(Debug, Default)]
pub struct FrameOutput {
    /// The mouse cursor to show over the view.
    pub cursor: egui::CursorIcon,
    /// Text a field copied (FigCraft's own Copy writes the clipboard itself).
    pub copied_text: Option<String>,
    /// A link to open in the browser.
    pub open_url: Option<String>,
    /// When the canvas wants its next frame (`Duration::MAX`: only on input).
    pub repaint_after: Duration,
    /// Selections and clicks since the last frame.
    pub events: Vec<FigEvent>,
    /// Seconds spent waiting for a drawable (vsync) this frame.
    pub vsync_wait: f32,
}

/// What [`FigEmbedded::open`] did.
#[derive(Clone, Debug)]
pub struct OpenReport {
    /// The board file (`<stem>.board.figcraft` next to the project).
    pub board: PathBuf,
    /// No board existed: a new one was made (and saved there).
    pub created: bool,
    /// `board.autoMain`'s answer when it ran (a new or empty board): `Err` with FigCraft's
    /// message, "unknown command …" while FigCraft doesn't have it yet.
    pub auto_main: Option<Result<Value, String>>,
}

/// A wgpu instance for the host to make its surface with (Metal only on macOS).
pub fn new_instance() -> wgpu::Instance {
    let setup = egui_wgpu::WgpuSetup::CreateNew(gpu::setup());
    pollster::block_on(setup.new_instance())
}

/// How often a command FigCraft doesn't know is tried again (the canvas commands land in FigCraft
/// while Ghostex already calls them).
const UNKNOWN_RETRY: Duration = Duration::from_secs(2);
/// A press that moves less than this (points) and is released is a click.
const CLICK_SLOP: f32 = 4.0;

/// FigCraft's canvas running inside a host view. See the crate docs.
pub struct FigEmbedded {
    ctx: egui::Context,
    surface: wgpu::Surface<'static>,
    rs: egui_wgpu::RenderState,
    needs_configure: bool,
    /// Drawable size in pixels and pixels per point.
    size: [u32; 2],
    ppp: f32,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    focused: bool,
    system_theme: Option<egui::Theme>,
    beginning: Instant,
    capture: Option<CaptureState>,
    capture_tx: CaptureSender,
    capture_rx: CaptureReceiver,
    painting: bool,
    pending_textures: egui::TexturesDelta,
    chrome: bool,
    queue: Arc<Mutex<Vec<ControlRequest>>>,
    control_port: Option<u16>,
    /// The project and board [`FigEmbedded::open`] opened last.
    project: Option<PathBuf>,
    board: Option<PathBuf>,
    /// Review marks already handed over (`canvas.annotate.list`), by id.
    marks_seen: HashSet<String>,
    marks_since: u64,
    /// The armed review tool: as `canvas.annotate.list` reports it, else as the host last set it.
    armed: Option<String>,
    /// Commands FigCraft didn't know, and when they were last tried.
    unknown: Vec<(String, Instant)>,
    /// The selection the host last heard of (node ids).
    last_selection: Vec<u64>,
    /// Where the primary button went down (points) and whether it moved since.
    press: Option<(egui::Pos2, bool)>,
    /// A click to report after the frame that handled its release.
    clicked: bool,
}

impl FigEmbedded {
    /// Start FigCraft drawing into `surface` (made from `instance`, see [`new_instance`]), `size`
    /// pixels at `pixels_per_point`. `wake` is called from any thread when the canvas wants a
    /// frame (`delay` from now): the host then calls [`FigEmbedded::run_frame`] on its thread.
    pub fn start(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        size: [u32; 2],
        pixels_per_point: f32,
        opts: EmbedOptions,
        wake: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Result<Self, String> {
        let config = egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(gpu::setup()), ..Default::default() };
        let rs = pollster::block_on(egui_wgpu::RenderState::create(&config, &instance, Some(&surface), egui_wgpu::RendererOptions::default()))
            .map_err(|e| format!("FigCraft found no usable GPU for the embedded canvas: {e}"))?;
        log::info!("embedded FigCraft: graphics {:?}, surface format {:?}", rs.adapter.get_info(), rs.target_format);

        let ctx = egui::Context::default();
        // FigCraft's other windows become windows inside the view: the host has no second native
        // window for them.
        ctx.set_embed_viewports(true);
        ctx.set_request_repaint_callback(move |info| wake(info.delay));

        // System fonts in the background; the canvas lays text out again once they're in.
        let fonts_ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("figcraft-fonts".into()).spawn(move || {
            figcraft_text::scan_system_fonts();
            fonts_ctx.request_repaint();
        });

        let mut session = figcraft_engine::Session::empty();
        if let Some(prefs) = services::read_prefs() {
            session.prefs = prefs;
        }
        // The board saves itself a moment after each change (FigCraft's auto-save).
        session.prefs.auto_save = true;
        figcraft_ui_egui::i18n::set(&session.prefs.language, None);
        let gpu_canvas = session.prefs.gpu_canvas;
        let mut fig = FigApp::new(session, Box::new(services::EmbedServices::new()));
        fig.ui.home = false;
        fig.ui.title_bar = figcraft_ui_egui::TitleBar::Native;
        if !opts.chrome {
            fig.ui.left_collapsed = true;
            fig.ui.right_collapsed = true;
        }
        figcraft_ui_egui::windows::install(fig);
        gpu::install_tiles(&rs, gpu_canvas);
        gpu::install_effectcraft(&rs);
        check_effectcraft_port(opts.effectcraft_port);

        let queue: Arc<Mutex<Vec<ControlRequest>>> = Arc::new(Mutex::new(Vec::new()));
        if let Some(port) = opts.control_port {
            control_server::start(port, queue.clone(), Arc::new(Mutex::new(Some(ctx.clone()))));
        }

        let (capture_tx, capture_rx) = capture_channel();
        Ok(Self {
            ctx,
            surface,
            rs,
            needs_configure: true,
            size: clamp_size(size),
            ppp: clamp_ppp(pixels_per_point, 2.0),
            events: Vec::new(),
            modifiers: egui::Modifiers::default(),
            focused: false,
            system_theme: None,
            beginning: Instant::now(),
            capture: None,
            capture_tx,
            capture_rx,
            painting: true,
            pending_textures: egui::TexturesDelta::default(),
            chrome: opts.chrome,
            queue,
            control_port: opts.control_port,
            project: None,
            board: None,
            marks_seen: HashSet::new(),
            marks_since: 0,
            armed: None,
            unknown: Vec::new(),
            last_selection: Vec::new(),
            press: None,
            clicked: false,
        })
    }

    /// Open the board of `ecproj`: `<stem>.board.figcraft` next to it, opened when it exists
    /// (already open: shown again), else made. A new or empty board (no EffectCraft card on it)
    /// is built with `board.autoMain {ecproj}`; a new one is saved at once, so auto-save keeps it.
    pub fn open(&mut self, ecproj: &Path) -> Result<OpenReport, String> {
        let board = board_path(ecproj);
        let ctx = self.ctx.clone();
        let report = with_app(|fig| board::open(fig, &ctx, ecproj, &board)).ok_or("FigCraft isn't running")??;
        self.project = Some(ecproj.to_path_buf());
        self.board = Some(board);
        self.last_selection.clear();
        self.ctx.request_repaint();
        Ok(report)
    }

    /// The project and board opened last.
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    pub fn board(&self) -> Option<&Path> {
        self.board.as_deref()
    }

    /// Run a FigCraft command in process, as the control channel's `engine.execute` does
    /// (programmatic: it never opens a dialog). A command FigCraft's engine doesn't know is tried
    /// as a control method of the same name (UI-level commands); still unknown, the engine's
    /// "unknown command" error comes back. The canvas redraws after it.
    pub fn execute(&mut self, command: &str, params: Value) -> Result<Value, String> {
        let ctx = self.ctx.clone();
        let r = with_app(|fig| run_command(fig, &ctx, command, &params)).unwrap_or_else(|| Err("FigCraft isn't running".into()));
        if r.is_ok() {
            match command {
                "canvas.annotate.start" => self.armed = params.get("tool").and_then(Value::as_str).map(str::to_string),
                "canvas.annotate.stop" => self.armed = None,
                _ => {}
            }
        }
        self.ctx.request_repaint();
        r
    }

    /// [`FigEmbedded::execute`] for calls made every frame or often: a command FigCraft doesn't
    /// know yet isn't asked again for [`UNKNOWN_RETRY`] (`None` meanwhile).
    pub fn execute_quiet(&mut self, command: &str, params: Value) -> Option<Result<Value, String>> {
        let now = Instant::now();
        if self.unknown.iter().any(|(c, at)| c == command && now.duration_since(*at) < UNKNOWN_RETRY) {
            return None;
        }
        let ctx = self.ctx.clone();
        let r = with_app(|fig| run_command(fig, &ctx, command, &params))?;
        self.unknown.retain(|(c, _)| c != command);
        if let Err(e) = &r
            && is_unknown(e)
        {
            self.unknown.push((command.to_string(), now));
        }
        Some(r)
    }

    /// The Pen / Comment / Layer marks added on the canvas since the last call
    /// (`canvas.annotate.list`, oldest first, each as EffectCraft's `annotate.add` returns one:
    /// comp, time, timecode, bbox, layers, note, copyRef), for the host to queue as chat cards. The
    /// marks stay on the canvas until the host removes them (`canvas.annotate.remove`).
    pub fn new_annotations(&mut self) -> Vec<Value> {
        let since = self.marks_since;
        let Some(Ok(listed)) = self.execute_quiet("canvas.annotate.list", json!({"since": since})) else {
            return Vec::new();
        };
        if let Some(armed) = listed.get("armed") {
            self.armed = armed.as_str().map(str::to_string);
        }
        let (fresh, max) = board::fresh_marks(&listed, &mut self.marks_seen);
        self.marks_since = self.marks_since.max(max);
        fresh
    }

    /// The armed review tool (`"pen"`, `"comment"`, `"layer"`), or `None`: the host's buttons
    /// follow it (Esc on the canvas disarms).
    pub fn annotate_armed(&self) -> Option<&str> {
        self.armed.as_deref()
    }

    /// The EffectCraft card selected on the canvas (a layer inside a card counts as it), if any.
    pub fn selected_card(&self) -> Option<CardRef> {
        with_app(|fig| board::selection(fig).0.into_iter().next()).flatten()
    }

    /// The port FigCraft's control server was asked to listen on.
    pub fn control_port(&self) -> Option<u16> {
        self.control_port
    }

    /// The egui context (for diagnostics; input goes through [`FigEmbedded::push`]).
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// The modifiers held, as last reported.
    pub fn modifiers(&self) -> egui::Modifiers {
        self.modifiers
    }

    /// The view changed size or moved to a display with another scale.
    pub fn resize(&mut self, size: [u32; 2], pixels_per_point: f32) {
        let size = clamp_size(size);
        let ppp = clamp_ppp(pixels_per_point, self.ppp);
        if size != self.size || (ppp - self.ppp).abs() > f32::EPSILON {
            self.size = size;
            self.ppp = ppp;
            self.needs_configure = true;
            self.ctx.request_repaint();
        }
    }

    /// Keyboard focus moved to (or away from) the view.
    pub fn focus(&mut self, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            self.events.push(egui::Event::WindowFocused(focused));
            self.ctx.request_repaint();
        }
    }

    /// The host shows (`true`) or hides the view. Hidden, [`FigEmbedded::run_frame`] still runs
    /// FigCraft's frame (auto-save, card links, control requests) but draws nothing; the view
    /// comes back exactly as it was.
    pub fn set_visible(&mut self, visible: bool) {
        self.painting = visible;
        if visible {
            self.ctx.request_repaint();
        }
    }

    /// The host's light/dark appearance.
    pub fn set_system_theme(&mut self, theme: Option<egui::Theme>) {
        self.system_theme = theme;
    }

    /// One input event, in points relative to the view's top-left corner.
    pub fn push(&mut self, event: egui::Event) {
        match &event {
            egui::Event::ModifiersChanged(m) => self.modifiers = *m,
            egui::Event::Key { modifiers, .. } | egui::Event::MouseWheel { modifiers, .. } => self.modifiers = *modifiers,
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers } => {
                self.modifiers = *modifiers;
                if *pressed {
                    self.press = Some((*pos, false));
                } else if let Some((at, moved)) = self.press.take() {
                    self.clicked = !moved && at.distance(*pos) <= CLICK_SLOP;
                }
            }
            egui::Event::PointerButton { modifiers, .. } => self.modifiers = *modifiers,
            egui::Event::PointerMoved(p) => {
                if let Some((at, moved)) = &mut self.press
                    && at.distance(*p) > CLICK_SLOP
                {
                    *moved = true;
                }
            }
            _ => {}
        }
        self.events.push(event);
        self.ctx.request_repaint();
    }

    /// A key from AppKit, turned into egui events as egui-winit makes them. `clipboard` is read
    /// only for ⌘V.
    pub fn push_mac_key(&mut self, key: MacKey<'_>, clipboard: impl FnOnce() -> Option<String>) {
        for e in keys::events(&key, clipboard) {
            self.push(e);
        }
    }

    /// Run one egui pass and draw it. Call when [`FrameOutput::repaint_after`] (or the `wake`
    /// callback) says so, or after input.
    pub fn run_frame(&mut self) -> FrameOutput {
        for (viewport_id, data, image) in self.capture_rx.try_iter() {
            let image = Arc::new(image);
            for user_data in data {
                self.events.push(egui::Event::Screenshot { viewport_id, user_data, image: image.clone() });
            }
        }
        let [w, h] = self.size;
        let points = egui::vec2(w as f32 / self.ppp, h as f32 / self.ppp);
        // The bare canvas: FigCraft lays its tab strip and left rail out above and left of the
        // drawable, so the canvas starts at the view's corner (its side panels are folded).
        let origin = if self.chrome { egui::Pos2::ZERO } else { egui::pos2(-board::RAIL_WIDTH, -figcraft_ui_egui::tabs::HEIGHT) };
        let screen = egui::Rect::from_min_max(origin, points.to_pos2());
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(self.beginning.elapsed().as_secs_f64()),
            focused: self.focused,
            events: std::mem::take(&mut self.events),
            max_texture_side: Some(self.rs.device.limits().max_texture_dimension_2d as usize),
            system_theme: self.system_theme,
            ..Default::default()
        };
        {
            let vp = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
            vp.native_pixels_per_point = Some(self.ppp);
            vp.focused = Some(self.focused);
            vp.inner_rect = Some(screen);
        }
        // Pointer and key events sent over the control channel (`ui.input`).
        if let Some(events) = with_app(|fig| figcraft_ui_egui::windows::take_input(fig, raw.viewport_id)) {
            raw.events.extend(events);
        }
        let queue = self.queue.clone();
        let full = self.ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            with_app(|fig| {
                control_server::drain_queue(&queue, &mut fig.control_queue);
                figcraft_ui_egui::windows::guard(fig, &ctx, |fig| fig.ui(ui));
            });
        });
        let mut out = FrameOutput { cursor: full.platform_output.cursor_icon, repaint_after: Duration::MAX, ..Default::default() };
        for c in &full.platform_output.commands {
            match c {
                egui::OutputCommand::CopyText(t) => out.copied_text = Some(t.clone()),
                egui::OutputCommand::OpenUrl(u) => out.open_url = Some(u.url.clone()),
                _ => {}
            }
        }
        if out.open_url.is_none() {
            out.open_url = services::take_url();
        }
        out.events = self.take_events();
        let mut capture_data = Vec::new();
        if let Some(vp) = full.viewport_output.get(&egui::ViewportId::ROOT) {
            out.repaint_after = vp.repaint_delay;
            for cmd in &vp.commands {
                if let egui::ViewportCommand::Screenshot(data) = cmd {
                    capture_data.push(data.clone());
                }
            }
        }
        let mut textures = std::mem::take(&mut self.pending_textures);
        textures.append(full.textures_delta);
        if !self.painting && capture_data.is_empty() {
            self.pending_textures = textures;
            return out;
        }
        let primitives = self.ctx.tessellate(full.shapes, full.pixels_per_point);
        out.vsync_wait = self.paint(full.pixels_per_point, &primitives, &mut textures, capture_data);
        out
    }

    /// The selection changes and the click since the last frame.
    fn take_events(&mut self) -> Vec<FigEvent> {
        let mut events = Vec::new();
        let state = with_app(|fig| {
            let (cards, nodes) = board::selection(fig);
            let ids: Vec<u64> = nodes.iter().map(|n| n.id).collect();
            let clicked = self.clicked.then(|| board::under_pointer(fig));
            (cards, nodes, ids, clicked)
        });
        self.clicked = false;
        let Some((cards, nodes, ids, clicked)) = state else {
            return events;
        };
        if ids != self.last_selection {
            self.last_selection = ids;
            events.push(FigEvent::Selection { cards, nodes });
        }
        if let Some((card, node)) = clicked {
            events.push(FigEvent::Clicked { card, node });
        }
        events
    }

    /// The host is going away (Ghostex quits or closes the Editing view): unsaved boards are
    /// saved where they live.
    pub fn shutdown(&mut self) {
        let _ = with_app(board::save_all);
    }

    fn configure(&mut self) {
        let [w, h] = self.size;
        let Some(mut config) = self.surface.get_default_config(&self.rs.adapter, w, h) else {
            log::error!("embedded FigCraft: the view's surface isn't supported by the adapter");
            return;
        };
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        config.format = self.rs.target_format;
        config.view_formats = vec![self.rs.target_format];
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        self.surface.configure(&self.rs.device, &config);
        self.needs_configure = false;
    }

    /// Upload textures and buffers, draw into the surface (through the capture texture when a
    /// screenshot was asked for) and present. Returns the seconds spent waiting for a drawable.
    fn paint(
        &mut self,
        ppp: f32,
        primitives: &[egui::ClippedPrimitive],
        textures: &mut egui::TexturesDelta,
        capture_data: Vec<egui::UserData>,
    ) -> f32 {
        let rs = self.rs.clone();
        let mut encoder = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("embedded figcraft") });
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: self.size, pixels_per_point: ppp };
        let user_cmd_bufs = {
            let mut renderer = rs.renderer.write();
            #[allow(clippy::iter_over_hash_type)] // order doesn't matter
            for (id, deltas) in textures.set.drain() {
                for delta in deltas {
                    renderer.update_texture(&rs.device, &rs.queue, id, &delta);
                }
            }
            renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, primitives, &screen)
        };
        if self.needs_configure {
            self.configure();
        }
        let started = Instant::now();
        let acquired = self.surface.get_current_texture();
        let vsync = started.elapsed().as_secs_f32();
        let output = match acquired {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.needs_configure = true;
                t
            }
            other => {
                log::debug!("embedded FigCraft: no drawable ({other:?}), reconfiguring");
                self.needs_configure = true;
                self.ctx.request_repaint();
                rs.queue.submit(user_cmd_bufs.into_iter().chain([encoder.finish()]));
                self.free(textures);
                return vsync;
            }
        };
        let capture = !capture_data.is_empty();
        let mut capture_buffer = None;
        {
            let renderer = rs.renderer.read();
            let target = if capture {
                let state = self.capture.get_or_insert_with(|| CaptureState::new(&rs.device, &output.texture));
                state.update(&rs.device, &output.texture);
                &state.texture
            } else {
                &output.texture
            };
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("embedded figcraft"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), primitives, &screen);
            if capture && let Some(state) = &mut self.capture {
                capture_buffer = Some(state.copy_textures(&rs.device, &output, &mut encoder));
            }
        }
        rs.queue.submit(user_cmd_bufs.into_iter().chain([encoder.finish()]));
        self.free(textures);
        if let (Some(buffer), Some(state)) = (capture_buffer, &self.capture) {
            state.read_screen_rgba(self.ctx.clone(), buffer, capture_data, self.capture_tx.clone(), egui::ViewportId::ROOT);
        }
        rs.queue.present(output);
        vsync
    }

    fn free(&self, textures: &mut egui::TexturesDelta) {
        let mut renderer = self.rs.renderer.write();
        #[allow(clippy::iter_over_hash_type)] // order doesn't matter
        for id in textures.free.drain() {
            renderer.free_texture(&id);
        }
    }
}

/// `command` through the control channel's `engine.execute`, else as a control method of that
/// name when the engine doesn't know it.
fn run_command(fig: &mut FigApp, ctx: &egui::Context, command: &str, params: &Value) -> Result<Value, String> {
    let r = figcraft_ui_egui::control::handle(fig, ctx, "engine.execute", &json!({"command": command, "params": params}));
    match r {
        Err(e) if is_unknown(&e) => match figcraft_ui_egui::control::handle(fig, ctx, command, params) {
            Err(other) if other.starts_with("unknown method") => Err(e),
            other => other,
        },
        other => other,
    }
}

/// FigCraft's "unknown command `x`" (the command isn't in this build yet).
pub fn is_unknown(error: &str) -> bool {
    error.starts_with("unknown command")
}

/// Cards mirror the EffectCraft on `EFFECTCRAFT_CONTROL_PORT` (else 9877): say so when the host
/// runs its own EffectCraft on another port.
fn check_effectcraft_port(wanted: Option<u16>) {
    let Some(wanted) = wanted else { return };
    let used = std::env::var("EFFECTCRAFT_CONTROL_PORT").ok().and_then(|p| p.trim().parse::<u16>().ok()).unwrap_or(9877);
    if used != wanted {
        log::warn!(
            "embedded FigCraft: cards follow the EffectCraft on port {used}, not the host's on {wanted} (set EFFECTCRAFT_CONTROL_PORT={wanted})"
        );
        eprintln!("embedded FigCraft: cards follow the EffectCraft on port {used}, not the host's on {wanted}");
    }
}

/// A drawable of at least 1 × 1 and at most 16384 px a side.
fn clamp_size([w, h]: [u32; 2]) -> [u32; 2] {
    [w.clamp(1, 16384), h.clamp(1, 16384)]
}

/// Pixels per point between 0.5 and 8 (`fallback` when not a number).
fn clamp_ppp(ppp: f32, fallback: f32) -> f32 {
    if ppp.is_finite() { ppp.clamp(0.5, 8.0) } else { fallback }
}
