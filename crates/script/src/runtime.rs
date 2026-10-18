//! The interpreter: the host hand-off while a script runs, the budget, output and errors.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;

use boa_engine::error::{EngineError, RuntimeLimitError};
use boa_engine::{Context, JsError, JsNativeError, JsResult, JsValue, Source, js_string};
use serde_json::{Value, json};

/// Lines `console.log` may keep, and their total size.
const MAX_LOG_LINES: usize = 1000;
const MAX_LOG_BYTES: usize = 256 * 1024;
/// The longest script accepted, and the largest result (as JSON text).
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;
/// Iterations of one loop, and nested calls: a runaway loop with no API calls in it ends here
/// (the time budget is checked on every API call).
const LOOP_LIMIT: u64 = 100_000_000;
const RECURSION_LIMIT: usize = 1000;

/// What a script sees of the session. Every write goes through [`Host::call`].
pub trait Host: Any {
    /// Run engine command `command` with `params` and return its result.
    fn call(&mut self, command: &str, params: &Value) -> Result<Value, String>;
    /// Layer `id` as JSON (the properties `node.get` reports, plus `parent` and `children` ids),
    /// or `None` when there is no such layer.
    fn node(&mut self, id: u64) -> Option<Value>;
    /// The layers under `id`, depth first in layer-panel order (top first), `id` left out.
    fn descendants(&mut self, id: u64) -> Vec<u64>;
    /// `{page, pages: [ids], selection: [ids]}` of the open document.
    fn context(&mut self) -> Value;
}

/// How to run a script.
#[derive(Clone, Debug)]
pub struct Options {
    /// The script's `args` global.
    pub args: Value,
    /// Wall-clock budget in seconds (checked on every API call).
    pub timeout: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { args: Value::Null, timeout: 10.0 }
    }
}

/// An uncaught error, with its position in the script (1-based).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptError {
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// What a run produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The script's `return` value as JSON (layers as their properties; `null` without one).
    pub result: Value,
    /// `console.log` lines.
    pub logs: Vec<String>,
    pub error: Option<ScriptError>,
    /// Engine commands the script ran.
    pub calls: u64,
}

impl Outcome {
    fn failed(message: impl Into<String>) -> Self {
        Self { error: Some(ScriptError { message: message.into(), line: None, column: None }), ..Self::default() }
    }

    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "result": self.result,
            "logs": self.logs,
            "calls": self.calls,
            "error": self.error.as_ref().map(|e| json!({"message": e.message, "line": e.line, "column": e.column})),
        })
    }
}

/// The host and per-run state while a script runs (the natives reach it here; nothing borrowed
/// ever enters the JS heap).
pub(crate) struct Active {
    host: Box<dyn Host>,
    logs: Vec<String>,
    log_bytes: usize,
    calls: u64,
    deadline: Option<web_time::Instant>,
    /// Set once the budget ran out: every later API call fails too (a `catch` can't keep going).
    stopped: Option<String>,
    /// Layers read since the last write.
    cache: HashMap<u64, Option<Value>>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

pub(crate) fn throw(message: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(message.into()).into()
}

/// Why an API call failed: an error the script may catch, or the end of the run (an engine
/// error, which `try … catch` can't catch, so a loop around it can't keep the run going).
enum Failure {
    Error(String),
    Stopped,
}

impl From<String> for Failure {
    fn from(s: String) -> Self {
        Failure::Error(s)
    }
}

impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Failure::Error(s.to_string())
    }
}

fn with_active<T>(f: impl FnOnce(&mut Active) -> Result<T, String>) -> JsResult<T> {
    let r = ACTIVE
        .try_with(|cell| {
            let mut slot = cell.try_borrow_mut().map_err(|_| "the script API was called while it was busy")?;
            let a = slot.as_mut().ok_or("no script is running")?;
            if a.stopped.is_some() {
                return Err(Failure::Stopped);
            }
            if a.deadline.is_some_and(|d| web_time::Instant::now() > d) {
                a.stopped = Some("the script ran out of time (raise `timeout`)".to_string());
                return Err(Failure::Stopped);
            }
            f(a).map_err(Failure::Error)
        })
        .unwrap_or(Err(Failure::Error("no script is running".into())));
    r.map_err(|e| match e {
        Failure::Error(m) => throw(m),
        Failure::Stopped => EngineError::from(RuntimeLimitError::LoopIteration).into(),
    })
}

/// Run command `command` (a write: the layer cache is dropped).
pub(crate) fn call(command: &str, params: &Value) -> JsResult<Value> {
    with_active(|a| {
        a.calls += 1;
        a.cache.clear();
        a.host.call(command, params).map_err(|e| format!("{command}: {e}"))
    })
}

/// Layer `id`'s properties, `None` when it doesn't exist.
pub(crate) fn node(id: u64) -> JsResult<Option<Value>> {
    with_active(|a| {
        if let Some(v) = a.cache.get(&id) {
            return Ok(v.clone());
        }
        let v = a.host.node(id);
        a.cache.insert(id, v.clone());
        Ok(v)
    })
}

pub(crate) fn descendants(id: u64) -> JsResult<Vec<u64>> {
    with_active(|a| Ok(a.host.descendants(id)))
}

pub(crate) fn context() -> JsResult<Value> {
    with_active(|a| Ok(a.host.context()))
}

pub(crate) fn log(line: String) -> JsResult<()> {
    with_active(|a| {
        if a.logs.len() >= MAX_LOG_LINES || a.log_bytes + line.len() > MAX_LOG_BYTES {
            if a.logs.last().is_none_or(|l| !l.starts_with("… output limit")) {
                a.logs.push(format!("… output limit reached ({MAX_LOG_LINES} lines or {} KB)", MAX_LOG_BYTES / 1024));
            }
            return Ok(());
        }
        a.log_bytes += line.len();
        a.logs.push(line);
        Ok(())
    })
}

/// Run `source` against `host`. The host comes back with the outcome; it is never lost, also not
/// when the script fails, runs out of time or the interpreter panics.
pub fn run(host: Box<dyn Host>, source: &str, opts: &Options) -> (Box<dyn Host>, Outcome) {
    if source.len() > MAX_SOURCE_BYTES {
        return (host, Outcome::failed(format!("the script is longer than {} KB", MAX_SOURCE_BYTES / 1024)));
    }
    let busy = ACTIVE.try_with(|c| c.try_borrow().map_or(true, |a| a.is_some())).unwrap_or(true);
    if busy {
        return (host, Outcome::failed("a script is already running (scripts can't run scripts)"));
    }
    let timeout = if opts.timeout.is_finite() && opts.timeout > 0.0 { opts.timeout.min(24.0 * 3600.0) } else { Options::default().timeout };
    let deadline = web_time::Instant::now().checked_add(web_time::Duration::from_secs_f64(timeout));
    let active = Active { host, logs: Vec::new(), log_bytes: 0, calls: 0, deadline, stopped: None, cache: HashMap::new() };
    let mut host_back = None;
    let _ = ACTIVE.try_with(|c| match c.try_borrow_mut() {
        Ok(mut slot) => *slot = Some(active),
        Err(_) => host_back = Some(active.host),
    });
    if let Some(host) = host_back {
        return (host, Outcome::failed("a script is already running (scripts can't run scripts)"));
    }

    let r = std::panic::catch_unwind(AssertUnwindSafe(|| execute(source, opts)));
    let taken = ACTIVE.try_with(|c| c.try_borrow_mut().ok().and_then(|mut s| s.take())).ok().flatten();
    let Some(a) = taken else {
        // Unreachable in practice (the slot is only borrowed inside an API call), but the host
        // can't be handed back without it.
        return (Box::new(Lost), Outcome::failed("the script lost its document"));
    };
    let result = match r {
        Ok(r) => r,
        Err(_) => Err(ScriptError { message: "the script engine failed (internal error)".into(), line: None, column: None }),
    };
    let (result, error) = match (result, a.stopped) {
        (_, Some(why)) => (Value::Null, Some(ScriptError { message: why, line: None, column: None })),
        (Ok(v), None) => (v, None),
        (Err(e), None) => (Value::Null, Some(e)),
    };
    (a.host, Outcome { result, logs: a.logs, error, calls: a.calls })
}

/// Stands in for a host that couldn't be recovered (see [`run`]).
struct Lost;

impl Host for Lost {
    fn call(&mut self, _: &str, _: &Value) -> Result<Value, String> {
        Err("no document".into())
    }
    fn node(&mut self, _: u64) -> Option<Value> {
        None
    }
    fn descendants(&mut self, _: u64) -> Vec<u64> {
        Vec::new()
    }
    fn context(&mut self) -> Value {
        Value::Null
    }
}

/// The script runs as the body of a function, so `return` hands back its result.
const WRAP_HEAD: &str = "(function () {\n";
const WRAP_TAIL: &str = "\n}).call(undefined)";

fn execute(source: &str, opts: &Options) -> Result<Value, ScriptError> {
    let mut ctx = Context::default();
    let limits = ctx.runtime_limits_mut();
    limits.set_loop_iteration_limit(LOOP_LIMIT);
    limits.set_recursion_limit(RECURSION_LIMIT);
    crate::model::install(&mut ctx, &opts.args).map_err(|e| ScriptError {
        message: format!("setting up the script API: {e}"),
        line: None,
        column: None,
    })?;
    let code = format!("{WRAP_HEAD}{source}{WRAP_TAIL}");
    let v = ctx.eval(Source::from_bytes(code.as_bytes())).map_err(|e| script_error(&e, &mut ctx, source))?;
    to_json(&v, &mut ctx).map_err(|e| script_error(&e, &mut ctx, source))
}

/// A value as JSON through `JSON.stringify`, so layers come out as their properties (`toJSON`).
pub(crate) fn to_json(v: &JsValue, ctx: &mut Context) -> JsResult<Value> {
    if v.is_undefined() {
        return Ok(Value::Null);
    }
    let text = stringify(v, ctx)?;
    let Some(text) = text else { return Ok(Value::Null) };
    if text.len() > MAX_RESULT_BYTES {
        return Err(throw(format!("the result is larger than {} MB", MAX_RESULT_BYTES / (1024 * 1024))));
    }
    serde_json::from_str(&text).map_err(|e| throw(format!("the result isn't JSON: {e}")))
}

/// `JSON.stringify(v)`, `None` for values it leaves out (functions, `undefined`).
pub(crate) fn stringify(v: &JsValue, ctx: &mut Context) -> JsResult<Option<String>> {
    let json = ctx.global_object().get(js_string!("JSON"), ctx)?;
    let f = json.as_object().and_then(|o| o.get(js_string!("stringify"), ctx).ok()).and_then(|f| f.as_callable());
    let Some(f) = f else { return Err(throw("JSON.stringify is missing")) };
    let out = f.call(&json, std::slice::from_ref(v), ctx)?;
    Ok(out.as_string().map(|s| s.to_std_string_escaped()))
}

/// A boa error as a message with its line and column in the user's script.
fn script_error(e: &JsError, ctx: &mut Context, source: &str) -> ScriptError {
    let display = e.to_string();
    let message = match e.as_opaque() {
        Some(v) => {
            let o = v.as_object();
            let field = |k, ctx: &mut Context| {
                o.as_ref()
                    .and_then(|o| o.get(k, ctx).ok())
                    .filter(|m| !m.is_undefined())
                    .and_then(|m| m.to_string(ctx).ok())
                    .map(|s| s.to_std_string_escaped())
            };
            match (field(js_string!("name"), ctx), field(js_string!("message"), ctx)) {
                (Some(n), Some(m)) if n != "Error" => format!("{n}: {m}"),
                (_, Some(m)) => m,
                _ => v.to_string(ctx).map(|s| s.to_std_string_escaped()).unwrap_or(display.clone()),
            }
        }
        None => {
            let first = display.lines().next().unwrap_or("").trim();
            let first = first.split(" at line ").next().unwrap_or(first).trim();
            first.strip_prefix("Error: ").unwrap_or(first).to_string()
        }
    };
    let (line, column) = position(&display, source);
    ScriptError { message, line, column }
}

/// `at line N, col M` (syntax errors) or the first `:line:col)` backtrace frame, moved back by the
/// wrapper's first line; else the first use of a name that isn't defined.
fn position(msg: &str, source: &str) -> (Option<u32>, Option<u32>) {
    let digits = |s: &str| s.split(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse::<u32>().ok());
    let in_source = |line: u32| line.checked_sub(1).filter(|l| *l >= 1 && *l as usize <= source.lines().count().max(1));
    if let Some(i) = msg.find("at line ") {
        let rest = msg.get(i + 8..).unwrap_or("");
        let line = digits(rest).and_then(in_source);
        let col = rest.find("col ").and_then(|j| rest.get(j + 4..)).and_then(digits);
        if line.is_some() {
            return (line, col);
        }
    }
    let undefined = msg.lines().next().and_then(|l| l.split("ReferenceError: ").nth(1)).and_then(|r| r.split(" is not defined").next());
    if let Some(at) = undefined.and_then(|name| first_use(name, source)) {
        return (Some(at.0), Some(at.1));
    }
    for l in msg.lines() {
        // `(<file>:12:5)`
        let Some(open) = l.rfind('(') else { continue };
        let mut it = l.get(open + 1..).unwrap_or("").trim_end_matches(')').rsplit(':');
        let (Some(c), Some(n)) = (it.next(), it.next()) else { continue };
        if let (Ok(c), Some(n)) = (c.parse::<u32>(), n.parse::<u32>().ok().and_then(in_source)) {
            return (Some(n), Some(c));
        }
    }
    (None, None)
}

/// The first use of `name` as a whole identifier in `source` (1-based line and column).
fn first_use(name: &str, source: &str) -> Option<(u32, u32)> {
    if name.is_empty() {
        return None;
    }
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    for (n, line) in source.lines().enumerate() {
        let b = line.as_bytes();
        let mut from = 0;
        while let Some(i) = line.get(from..).and_then(|rest| rest.find(name)).map(|i| i + from) {
            let before = i.checked_sub(1).and_then(|j| b.get(j).copied());
            let after = b.get(i + name.len()).copied();
            if !before.is_some_and(|c| ident(c) || c == b'.') && !after.is_some_and(ident) {
                return Some((u32::try_from(n + 1).ok()?, u32::try_from(i + 1).ok()?));
            }
            from = i + name.len();
        }
    }
    None
}
