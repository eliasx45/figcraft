//! Fork-only (`effectcraft-inproc`): the hosted Timeline editing a project the EffectCraft app has
//! open. The app is the one source of truth; the panel's in-process session is a **mirror** of
//! the app's project.
//!
//! **Decision.** Forwarding each command the panels run doesn't work: EffectCraft's panels also
//! edit their session directly (`session.execute` from inside the Timeline, the Graph Editor,
//! markers; selection set in place), and selection-relative commands would act on the app's
//! selection, not the panel's. So the panels edit the mirror as if it were their own project,
//! and what changed travels as a project patch, the same comp/layer patches EffectCraft's cloud
//! preview keeps its replica with (`effectcraft_engine::remote_sync`):
//!
//! - **Pull.** `project.mirror {client: "figcraft"}` brings this side's copy of the app's
//!   project (`base`) to the app's revision: the whole project the first time, then patches of
//!   the comps and layers that changed. When the panel has no edit of its own pending, the
//!   session's project becomes that copy (the panel and the card show the app's edits live,
//!   asked every [`PULL_EVERY`]).
//! - **Push.** An edit in the panel (any change of the session's project: Timeline, Graph Editor,
//!   Effect Controls, the card's layer handles) is diffed against `base` and sent as
//!   `project.applyPatch {patch, label, merge}`: one undo step in the app, labelled as the panel's
//!   own undo step, consecutive pushes of one gesture folded (`merge`), marked unsaved there and
//!   drawn there at once. Then the app's result is pulled back, so both hold the app's project.
//!   One request is out at a time; edits made meanwhile go in the next push.
//! - **Undo / redo / save** are the app's: `edit.undo`, `edit.redo` and `file.save` are run in the
//!   app (its history, its file), never on the mirror.
//! - **The app wins.** A push made from a revision the app has moved on from (Elias edited there
//!   meanwhile) is refused by the app and the panel's edit is dropped; the mirror takes the app's
//!   project. A pull that brings app changes while a panel edit waits drops that edit too. A
//!   replica that can't be patched (a hash mismatch) asks for the whole project again.
//! - **Read-only fallback.** Until the first pull, while the app doesn't answer (closed, busy),
//!   when it has another project open, or when it's an EffectCraft without `project.mirror`, the
//!   panel is read-only (edits are taken back), retried every [`RETRY_EVERY`].
//!
//! Requests run on a thread of their own ([`Threaded`]) over the app's control channel
//! ([`Tcp`]); tests drive the same state machine synchronously ([`Direct`]) against an
//! in-process EffectCraft session standing in for the app.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::Project;
use figcraft_ec_inproc::effectcraft_engine::remote_sync::{self, Patch, SyncCache};
use serde_json::{Value, json};

/// The name the app knows this mirror by (its replica record, its undo steps' author).
pub const CLIENT: &str = "figcraft";
/// Seconds between pulls while the app answers.
pub const PULL_EVERY: Duration = Duration::from_millis(250);
/// And while it doesn't (or can't mirror).
pub const RETRY_EVERY: Duration = Duration::from_secs(2);
/// The commands the app runs itself instead of the mirror.
pub const FORWARDED: [&str; 3] = ["edit.undo", "edit.redo", "file.save"];

pub const CONNECTING: &str = "Connecting to EffectCraft…";
pub const DOWN: &str = "EffectCraft isn't answering — edits paused";
pub const ELSEWHERE: &str = "EffectCraft has another project open now — reopen this Timeline";
pub const DROPPED: &str = "EffectCraft changed the project meanwhile: that edit was dropped";

/// A failed request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// The app can't be reached (closed, the connection dropped, timed out).
    Down(String),
    /// The app answered with an error.
    Refused(String),
}

/// The app's engine: `engine.execute {command, params}` on its control channel.
pub trait Link {
    fn call(&mut self, command: &str, params: Value) -> Result<Value, LinkError>;
}

/// One request to the app (each ends with a pull).
#[derive(Clone, Debug)]
pub enum Job {
    Pull { reset: bool },
    Push { patch: Value, label: String, merge: String },
    Run { command: String, params: Value },
}

/// What a request came back with.
#[derive(Clone, Debug)]
pub enum Reply {
    Done {
        /// `project.mirror`'s answer.
        sync: Value,
        /// The push's outcome: the app's revision with it (`Err`: refused, the edit wasn't
        /// applied).
        pushed: Option<Result<u64, String>>,
        /// A forwarded command's error.
        ran: Option<Result<(), String>>,
    },
    /// The app can't mirror (`project.mirror` refused: an older EffectCraft).
    Unsupported(String),
    Down(String),
}

/// `command` run in the app as the user's own ⌘Z would: EffectCraft refuses a plain `edit.undo`
/// from the control channel when another client made the last step, and the panel's pushes are
/// client `figcraft`'s, so ⌘Z in the panel undid nothing of its own edits ("nothing of you to
/// undo"). `any: true` undoes the last step whoever made it, as ⌘Z in the app does.
fn as_the_user(command: &str, mut params: Value) -> Value {
    if command == "edit.undo" {
        if !params.is_object() {
            params = json!({});
        }
        if let Some(o) = params.as_object_mut() {
            o.entry("any").or_insert(Value::Bool(true));
        }
    }
    params
}

/// Run `job` against the app.
pub fn perform(link: &mut dyn Link, job: Job) -> Reply {
    let (mut pushed, mut ran, mut reset) = (None, None, false);
    match job {
        Job::Pull { reset: r } => reset = r,
        Job::Push { patch, label, merge } => {
            match link.call("project.applyPatch", json!({"client": CLIENT, "patch": patch, "label": label, "merge": merge})) {
                Ok(v) => pushed = Some(Ok(v.get("revision").and_then(Value::as_u64).unwrap_or(0))),
                Err(LinkError::Refused(e)) => pushed = Some(Err(e)),
                Err(LinkError::Down(e)) => return Reply::Down(e),
            }
        }
        // As the user would: no `client` (⌘Z undoes the app's last step, whoever made it).
        Job::Run { command, params } => match link.call(&command, as_the_user(&command, params)) {
            Ok(_) => ran = Some(Ok(())),
            Err(LinkError::Refused(e)) => ran = Some(Err(e)),
            Err(LinkError::Down(e)) => return Reply::Down(e),
        },
    }
    match link.call("project.mirror", json!({"client": CLIENT, "reset": reset})) {
        Ok(sync) => Reply::Done { sync, pushed, ran },
        Err(LinkError::Refused(e)) => Reply::Unsupported(e),
        Err(LinkError::Down(e)) => Reply::Down(e),
    }
}

/// How jobs reach the app.
pub trait Transport {
    /// `false`: it can't take jobs any more.
    fn send(&mut self, job: Job) -> bool;
    fn try_recv(&mut self) -> Option<Reply>;
}

/// Jobs run on a thread of their own (the app's answers never hold up FigCraft's frame).
pub struct Threaded {
    tx: Sender<Job>,
    rx: Receiver<Reply>,
}

impl Threaded {
    /// A thread running jobs on `link`; `ctx` repaints when an answer arrives.
    pub fn start(mut link: Box<dyn Link + Send>, ctx: Option<egui::Context>) -> Option<Threaded> {
        let (jtx, jrx) = channel::<Job>();
        let (rtx, rrx) = channel::<Reply>();
        std::thread::Builder::new()
            .name("effectcraft-mirror".into())
            .spawn(move || {
                while let Ok(job) = jrx.recv() {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| perform(link.as_mut(), job)))
                        .unwrap_or_else(|_| Reply::Down("EffectCraft mirror: an internal error".into()));
                    if rtx.send(r).is_err() {
                        break;
                    }
                    if let Some(c) = &ctx {
                        c.request_repaint();
                    }
                }
            })
            .ok()?;
        Some(Threaded { tx: jtx, rx: rrx })
    }
}

impl Transport for Threaded {
    fn send(&mut self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }
    fn try_recv(&mut self) -> Option<Reply> {
        self.rx.try_recv().ok()
    }
}

/// Jobs run at once, in the caller (tests).
#[cfg_attr(not(test), allow(dead_code))]
pub struct Direct {
    pub link: Box<dyn Link>,
    out: VecDeque<Reply>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Direct {
    pub fn new(link: Box<dyn Link>) -> Direct {
        Direct { link, out: VecDeque::new() }
    }
}

impl Transport for Direct {
    fn send(&mut self, job: Job) -> bool {
        let r = perform(self.link.as_mut(), job);
        self.out.push_back(r);
        true
    }
    fn try_recv(&mut self) -> Option<Reply> {
        self.out.pop_front()
    }
}

/// The app's control channel (127.0.0.1, `EFFECTCRAFT_CONTROL_PORT` or 9877), connected on use
/// and again after a failure.
pub struct Tcp {
    port: u16,
    conn: Option<(TcpStream, BufReader<TcpStream>)>,
    next: u64,
}

impl Tcp {
    pub fn new() -> Tcp {
        let port = std::env::var("EFFECTCRAFT_CONTROL_PORT").ok().and_then(|p| p.trim().parse().ok()).unwrap_or(9877);
        Tcp { port, conn: None, next: 1 }
    }

    fn connect(&mut self) -> Result<(), LinkError> {
        if self.conn.is_some() {
            return Ok(());
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).map_err(|e| LinkError::Down(format!("EffectCraft: {e}")))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = s.set_nodelay(true);
        let r = BufReader::new(s.try_clone().map_err(|e| LinkError::Down(format!("EffectCraft: {e}")))?);
        self.conn = Some((s, r));
        Ok(())
    }

    fn exchange(&mut self, line: &str, id: u64) -> Result<Value, LinkError> {
        let (w, r) = self.conn.as_mut().ok_or_else(|| LinkError::Down("EffectCraft: not connected".into()))?;
        w.write_all(line.as_bytes()).map_err(|e| LinkError::Down(format!("EffectCraft: {e}")))?;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = r.read_line(&mut buf).map_err(|e| LinkError::Down(format!("EffectCraft: {e}")))?;
            if n == 0 {
                return Err(LinkError::Down("EffectCraft closed the connection".into()));
            }
            // Event lines and other replies are skipped.
            let Ok(v) = serde_json::from_str::<Value>(&buf) else { continue };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            let e = v.get("error").and_then(Value::as_str).unwrap_or("EffectCraft refused the request");
            return Err(LinkError::Refused(e.to_string()));
        }
    }
}

impl Default for Tcp {
    fn default() -> Tcp {
        Tcp::new()
    }
}

impl Link for Tcp {
    fn call(&mut self, command: &str, params: Value) -> Result<Value, LinkError> {
        self.connect()?;
        let id = self.next;
        self.next += 1;
        let mut line = json!({"id": id, "method": "engine.execute", "params": {"command": command, "params": params}}).to_string();
        line.push('\n');
        let r = self.exchange(&line, id);
        if matches!(r, Err(LinkError::Down(_))) {
            self.conn = None;
        }
        r
    }
}

/// Where the mirror stands with the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Before the first pull.
    Connecting,
    /// In step with the app: the panel edits.
    Live,
    /// The app doesn't answer (`why`).
    Down(String),
    /// The app has another project open.
    Elsewhere,
    /// The app can't mirror.
    Unsupported(String),
}

/// What one [`Mirror::sync`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The app's project changed (a push landed, or its own edits arrived): its frames are new.
    pub app_changed: bool,
    /// Something to tell the user.
    pub status: Option<String>,
}

fn ptr(p: &Arc<Project>) -> usize {
    Arc::as_ptr(p) as usize
}

/// The session's link to the app's project (see the module docs).
pub struct Mirror {
    transport: Box<dyn Transport>,
    /// The project file's path (canonical): the app must have this one open.
    path: PathBuf,
    pub state: State,
    /// The app's project as last pulled, and its revision.
    base: Option<(u64, Arc<Project>)>,
    cache: SyncCache,
    /// The project last put into the session or pushed from it: anything else is a panel edit.
    known: Option<Arc<Project>>,
    /// The session's project when last looked at (each change counts once).
    seen: usize,
    /// What's out: a push (of this project), or another request.
    in_flight: Option<Option<Arc<Project>>>,
    /// Commands for the app ([`FORWARDED`]), in order.
    queue: VecDeque<(String, Value)>,
    /// Undo steps: a number per panel step (pushes of one step share a merge key) and the
    /// panel's top undo entry it was counted at.
    step: u64,
    step_top: Option<usize>,
    label: String,
    /// Makes this panel's merge keys its own (never folding into an earlier panel's step).
    nonce: u64,
    /// The highest id the app ever handed out.
    id_floor: u64,
    last_ask: Option<Instant>,
    pub pull_every: Duration,
    pub retry_every: Duration,
}

impl Mirror {
    pub fn new(path: &Path, transport: Box<dyn Transport>) -> Mirror {
        let nonce =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64) ^ u64::from(std::process::id());
        Mirror {
            transport,
            path: figcraft_ec_inproc::key(path),
            state: State::Connecting,
            base: None,
            cache: SyncCache::default(),
            known: None,
            seen: 0,
            in_flight: None,
            queue: VecDeque::new(),
            step: 0,
            step_top: None,
            label: String::new(),
            nonce,
            id_floor: 0,
            last_ask: None,
            pull_every: PULL_EVERY,
            retry_every: RETRY_EVERY,
        }
    }

    pub fn live(&self) -> bool {
        self.state == State::Live
    }

    /// Why the panel can't edit now (`None`: it can).
    pub fn read_only(&self) -> Option<String> {
        match &self.state {
            State::Live => None,
            State::Connecting => Some(CONNECTING.into()),
            State::Down(_) => Some(DOWN.into()),
            State::Elsewhere => Some(ELSEWHERE.into()),
            State::Unsupported(_) => Some(super::READ_ONLY.into()),
        }
    }

    /// Run `command` in the app (undo, redo, save), after any edit not sent yet.
    pub fn forward(&mut self, command: &str, params: Value) -> Result<(), String> {
        if !self.live() {
            return Err(self.read_only().unwrap_or_else(|| DOWN.into()));
        }
        self.queue.push_back((command.to_string(), params));
        Ok(())
    }

    /// Take `project.mirror`'s answer into `base`. `Ok(true)`: the app's project changed.
    fn take(&mut self, v: &Value) -> Result<bool, String> {
        let rev = v.get("revision").and_then(Value::as_u64).ok_or("project.mirror: no revision")?;
        if let Some(path) = v.get("path").and_then(Value::as_str)
            && figcraft_ec_inproc::key(Path::new(path)) != self.path
        {
            return Err("elsewhere".into());
        }
        if v.get("path").is_some_and(Value::is_null) {
            return Err("elsewhere".into());
        }
        self.id_floor = self.id_floor.max(v.get("idFloor").and_then(Value::as_u64).unwrap_or(0));
        if let Some(text) = v.get("project").and_then(Value::as_str) {
            let p = remote_sync::read_project(text.as_bytes())?;
            self.cache.clear();
            self.base = Some((rev, Arc::new(p)));
            return Ok(true);
        }
        let (from, base) = self.base.as_mut().ok_or("project.mirror: no replica to patch")?;
        if let Some(patch) = v.get("patch") {
            let patch: Patch = serde_json::from_value(patch.clone()).map_err(|e| e.to_string())?;
            if patch.from_rev != *from {
                return Err(format!("project.mirror: a patch from {} for a replica at {}", patch.from_rev, from));
            }
            remote_sync::apply(base, &patch.ops, &mut self.cache)?;
            *from = rev;
            // (a revision without a change of content: a view setting)
            return Ok(!patch.ops.is_empty());
        }
        if v.get("unchanged").and_then(Value::as_bool) == Some(true) {
            let moved = *from != rev;
            *from = rev;
            return Ok(moved);
        }
        Err("project.mirror: an answer without a project".into())
    }

    /// Take back a panel edit made while read-only: the app's project as last seen. `false`
    /// when there's none yet.
    pub fn revert(&mut self, s: &mut Session) -> bool {
        if self.base.is_none() {
            return false;
        }
        self.adopt(s);
        true
    }

    /// The session shows the app's project (`base`); a panel edit not sent is dropped.
    fn adopt(&mut self, s: &mut Session) {
        let Some((_, base)) = &self.base else { return };
        if !Arc::ptr_eq(&s.project, base) {
            s.project = base.clone();
            s.sanitize_state();
            s.bump();
        }
        self.known = Some(base.clone());
        self.seen = ptr(base);
    }

    /// The session's project changed since last looked at: count the panel's undo step.
    fn note_edit(&mut self, s: &Session) {
        let now = ptr(&s.project);
        if now == self.seen {
            return;
        }
        self.seen = now;
        let top = s.history.undo.last().map(|(_, p)| ptr(p));
        if top != self.step_top || s.history.merge_key.is_none() || self.step == 0 {
            self.step += 1;
            self.step_top = top;
            self.label = s.history.undo.last().map(|(l, _)| l.clone()).unwrap_or_default();
        }
    }

    /// The panel edited the session since it last showed the app's project.
    fn edited(&self, s: &Session) -> bool {
        self.known.as_ref().is_some_and(|k| !Arc::ptr_eq(k, &s.project))
    }

    /// One step, each frame: take what the app answered, then send what's next (a push of the
    /// panel's edits, a forwarded command, or a pull now and then).
    pub fn sync(&mut self, s: &mut Session) -> Outcome {
        let mut out = Outcome::default();
        // Edits not taken while read-only are taken back by the host; this counts the rest.
        if self.live() {
            self.note_edit(s);
        }
        while let Some(reply) = self.transport.try_recv() {
            let pushed_from = self.in_flight.take().flatten();
            match reply {
                Reply::Down(e) => {
                    if !matches!(self.state, State::Down(_)) {
                        log::warn!("EffectCraft mirror: {e}");
                    }
                    self.state = State::Down(e);
                    // What the panel did since can't reach the app: the app's project, as last seen.
                    self.adopt(s);
                }
                Reply::Unsupported(e) => {
                    log::warn!("EffectCraft mirror: the app can't mirror ({e}): read-only");
                    self.state = State::Unsupported(e);
                    self.adopt(s);
                }
                Reply::Done { sync, pushed, ran } => {
                    if let Some(Err(e)) = ran {
                        out.status = Some(e);
                    }
                    let refused = matches!(pushed, Some(Err(_)));
                    if let Some(Err(e)) = &pushed {
                        log::warn!("EffectCraft mirror: push refused: {e}");
                        out.status = Some(DROPPED.into());
                    }
                    if let Some(Ok(_)) = pushed {
                        out.app_changed = true;
                    }
                    // The app moved on its own between the push and the pull.
                    let moved_after_push = matches!(pushed, Some(Ok(r)) if r != 0 && sync.get("revision").and_then(Value::as_u64) != Some(r));
                    match self.take(&sync) {
                        Err(e) if e == "elsewhere" => {
                            self.state = State::Elsewhere;
                            self.adopt(s);
                        }
                        Err(e) => {
                            // The replica can't be trusted: the whole project next time.
                            log::warn!("EffectCraft mirror: {e}; asking for the whole project");
                            self.base = None;
                            self.last_ask = None;
                        }
                        Ok(changed) => {
                            let first = self.state != State::Live;
                            self.state = State::Live;
                            out.app_changed |= changed;
                            // Panel edits made since this request went out (or since the push it
                            // answers) still wait to be sent, unless the app moved under them.
                            let waiting = match &pushed_from {
                                Some(p) => !Arc::ptr_eq(p, &s.project),
                                None => self.edited(s),
                            };
                            let app_moved = (changed && pushed_from.is_none()) || moved_after_push;
                            if first || refused || !waiting || app_moved {
                                if waiting && !first && (refused || app_moved) {
                                    out.status = Some(DROPPED.into());
                                }
                                self.adopt(s);
                            }
                        }
                    }
                }
            }
        }
        // Ids continue past the app's (never one it handed out).
        s.id_floor = s.id_floor.max(self.id_floor);
        if self.in_flight.is_none() {
            self.next(s);
        }
        out
    }

    fn next(&mut self, s: &mut Session) {
        if self.live()
            && self.edited(s)
            && let Some((rev, base)) = &self.base
        {
            match remote_sync::diff_projects(base, &s.project, &mut self.cache) {
                Ok(ops) if ops.is_empty() => {
                    self.known = Some(s.project.clone());
                }
                Ok(ops) => {
                    let patch = Patch { from_rev: *rev, to_rev: rev.saturating_add(1), ops };
                    let label = if self.label.is_empty() { "Edit in FigCraft".to_string() } else { self.label.clone() };
                    let job = Job::Push {
                        patch: serde_json::to_value(&patch).unwrap_or(Value::Null),
                        label,
                        merge: format!("figcraft-{:x}-{}", self.nonce, self.step),
                    };
                    let sent = s.project.clone();
                    self.known = Some(sent.clone());
                    self.send(job, Some(sent));
                    return;
                }
                Err(e) => {
                    log::warn!("EffectCraft mirror: can't diff the edit ({e}); taking the app's project");
                    self.adopt(s);
                }
            }
        }
        if self.live()
            && let Some((command, params)) = self.queue.pop_front()
        {
            self.send(Job::Run { command, params }, None);
            return;
        }
        let every = if self.live() { self.pull_every } else { self.retry_every };
        let due = self.last_ask.is_none_or(|t| t.elapsed() >= every);
        if due && !matches!(self.state, State::Unsupported(_) | State::Elsewhere) {
            self.send(Job::Pull { reset: self.base.is_none() || !self.live() }, None);
        }
    }

    fn send(&mut self, job: Job, pushed: Option<Arc<Project>>) {
        self.last_ask = Some(Instant::now());
        if self.transport.send(job) {
            self.in_flight = Some(pushed);
        } else {
            self.state = State::Down("the mirror thread stopped".into());
        }
    }
}

#[cfg(test)]
mod as_the_user_tests {
    use super::{Job, Link, LinkError, perform};
    use serde_json::{Value, json};

    /// Records what the app was asked.
    struct Rec(Vec<(String, Value)>);
    impl Link for Rec {
        fn call(&mut self, command: &str, params: Value) -> Result<Value, LinkError> {
            self.0.push((command.to_string(), params));
            Ok(json!({"revision": 1}))
        }
    }

    /// Regression (live test 26 Oct, TESTER): ⌘Z in the mirrored Timeline after an edit made
    /// there did nothing: the app answered "nothing of you to undo: the last change was made by
    /// client `figcraft`". The undo goes as the user's, whoever made the step.
    #[test]
    fn undo_from_the_panel_undoes_whoever_made_the_last_step() {
        let mut link = Rec(Vec::new());
        let _ = perform(&mut link, Job::Run { command: "edit.undo".into(), params: json!({}) });
        let _ = perform(&mut link, Job::Run { command: "edit.undo".into(), params: Value::Null });
        let _ = perform(&mut link, Job::Run { command: "edit.redo".into(), params: json!({}) });
        let _ = perform(&mut link, Job::Run { command: "file.save".into(), params: json!({}) });
        let sent: Vec<&(String, Value)> = link.0.iter().filter(|(c, _)| c != "project.mirror").collect();
        assert_eq!(sent[0], &("edit.undo".to_string(), json!({"any": true})));
        assert_eq!(sent[1], &("edit.undo".to_string(), json!({"any": true})));
        assert_eq!(sent[2], &("edit.redo".to_string(), json!({})));
        assert_eq!(sent[3], &("file.save".to_string(), json!({})));
    }
}
