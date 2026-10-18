//! Scripts: JavaScript run against the open document (`figcraft-script`), for agents that need to
//! loop, branch and compute over many layers in one call. This module is the policy: a run is one
//! undo step ("Script: <label>"), rolled back as a whole when the script fails; scripts call the
//! same commands as the menus, MCP and plugins, but never open dialogs or run other scripts. A
//! failed script is kept for a while under an `editId`, so the next call can send only the fix
//! (`edits`) instead of the whole script again. See `docs/scripting.md`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use figcraft_doc::{Document, NodeId};
use serde_json::{Value, json};

use crate::cmd::CommandSpec;
use crate::params::{get, num, string};
use crate::{EngineError, Result, Session, bad, spec};

const CMD: &str = "script.run";
/// Failed scripts kept for `editId`.
const KEEP_FAILED: usize = 16;
/// Longest wall-clock budget a caller may ask for (seconds).
const MAX_TIMEOUT: f64 = 3600.0;
/// Ids listed per kind of change in a run's report (the counts are always exact).
const MAX_LISTED: usize = 1000;

/// Failed scripts of this session, newest last.
#[derive(Default)]
pub struct Scripts {
    failed: VecDeque<(String, String)>,
    next: u64,
}

impl Scripts {
    fn keep(&mut self, source: String) -> String {
        self.next += 1;
        let id = format!("s{}", self.next);
        self.failed.push_back((id.clone(), source));
        while self.failed.len() > KEEP_FAILED {
            self.failed.pop_front();
        }
        id
    }

    fn source(&self, id: &str) -> Option<&str> {
        self.failed.iter().find(|(i, _)| i == id).map(|(_, s)| s.as_str())
    }
}

/// The script's view of the session while it runs (the session is handed to the script for the
/// run and comes back after it).
struct ScriptHost {
    s: Rc<RefCell<Session>>,
}

impl figcraft_script::Host for ScriptHost {
    fn call(&mut self, command: &str, params: &Value) -> std::result::Result<Value, String> {
        if command.starts_with("script.") {
            return Err("scripts can't run scripts".into());
        }
        let mut s = self.s.try_borrow_mut().map_err(|_| "the session is busy".to_string())?;
        let ui_before = s.ui_requests.len();
        s.grouped(command, |s| {
            let v = s.execute(command, params)?;
            let opened_ui = s.ui_requests.len() > ui_before;
            s.ui_requests.truncate(ui_before);
            if opened_ui {
                return Err(EngineError::Disabled(command.into(), "it opens a dialog or panel, which scripts can't".into()));
            }
            Ok(v)
        })
        .map_err(|e| e.to_string())
    }

    fn node(&mut self, id: u64) -> Option<Value> {
        let s = self.s.try_borrow().ok()?;
        let doc = s.doc().ok()?;
        let n = doc.node(NodeId(id))?;
        let mut v = crate::inspect::node_json(doc, n.id, 0);
        let o = v.as_object_mut()?;
        o.remove("childCount");
        o.insert("parent".into(), json!(n.parent));
        o.insert("children".into(), json!(n.children));
        Some(v)
    }

    fn descendants(&mut self, id: u64) -> Vec<u64> {
        let Ok(s) = self.s.try_borrow() else { return Vec::new() };
        let Ok(doc) = s.doc() else { return Vec::new() };
        let mut out = Vec::new();
        // Depth first, children in their order; an explicit stack (no recursion on document depth).
        let mut stack: Vec<NodeId> = doc.node(NodeId(id)).map(|n| n.children.iter().rev().copied().collect()).unwrap_or_default();
        while let Some(c) = stack.pop() {
            let Some(n) = doc.node(c) else { continue };
            out.push(c.0);
            stack.extend(n.children.iter().rev().copied());
        }
        out
    }

    fn context(&mut self) -> Value {
        let Ok(s) = self.s.try_borrow() else { return Value::Null };
        match s.ds() {
            Ok(ds) => json!({"page": ds.page, "pages": ds.doc.pages, "selection": ds.selection}),
            Err(_) => Value::Null,
        }
    }
}

/// Apply `edits` (`[{find, replace, all?}]`) to `source`, in order.
fn apply_edits(source: &str, edits: &Value) -> Result<String> {
    let list = edits.as_array().ok_or_else(|| bad(CMD, "`edits` must be an array of {find, replace, all?}"))?;
    if list.is_empty() {
        return Err(bad(CMD, "`edits` is empty"));
    }
    let mut out = source.to_string();
    for (i, e) in list.iter().enumerate() {
        let find = string(e, "find").filter(|f| !f.is_empty()).ok_or_else(|| bad(CMD, format!("edit {i}: `find` must be a non-empty string")))?;
        let replace = string(e, "replace").ok_or_else(|| bad(CMD, format!("edit {i}: `replace` must be a string")))?;
        let all = get(e, "all").and_then(Value::as_bool).unwrap_or(false);
        let count = out.matches(find.as_str()).count();
        if count == 0 {
            return Err(bad(CMD, format!("edit {i}: `find` doesn't occur in the script")));
        }
        if count > 1 && !all {
            return Err(bad(CMD, format!("edit {i}: `find` occurs {count} times; make it unique or pass all: true")));
        }
        out = out.replace(find.as_str(), &replace);
    }
    Ok(out)
}

/// Created, changed and removed layers between two versions of a document.
fn changes(before: &Document, after: &Document) -> Value {
    let (mut created, mut changed, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    for id in after.changed_ids(before, usize::MAX).unwrap_or_default() {
        match (before.contains(id), after.contains(id)) {
            (false, true) => created.push(id),
            (true, false) => removed.push(id),
            _ => changed.push(id),
        }
    }
    // Only the top of what was made: a new frame's children come with it.
    let top: Vec<NodeId> =
        created.iter().copied().filter(|id| after.node(*id).and_then(|n| n.parent).is_none_or(|p| !created.contains(&p))).collect();
    let list = |v: &[NodeId]| json!(v.iter().take(MAX_LISTED).collect::<Vec<_>>());
    json!({
        "created": list(&created),
        "createdTop": list(&top),
        "changed": list(&changed),
        "removed": list(&removed),
        "counts": {"created": created.len(), "changed": changed.len(), "removed": removed.len()},
    })
}

/// Hand the session to `figcraft_script::run` and take it back.
fn run_on(s: &mut Session, source: &str, opts: &figcraft_script::Options) -> figcraft_script::Outcome {
    let shared = Rc::new(RefCell::new(std::mem::replace(s, Session::empty())));
    let (host, outcome) = figcraft_script::run(Box::new(ScriptHost { s: shared.clone() }), source, opts);
    drop(host);
    *s = match Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        // Only this function holds it once the host is dropped; take it out anyway.
        Err(rc) => rc.try_borrow_mut().map(|mut b| std::mem::replace(&mut *b, Session::empty())).unwrap_or_else(|_| Session::empty()),
    };
    outcome
}

/// `script.run`: one undo step, all or nothing.
fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let text = |k: &str| match get(p, k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad(CMD, format!("`{k}` must be a string"))),
    };
    let source = match (text("source")?, text("editId")?) {
        (Some(src), None) => src,
        (None, Some(id)) => {
            let old = s
                .scripts
                .source(&id)
                .ok_or_else(|| bad(CMD, format!("no failed script `{id}` (they are kept for the last {KEEP_FAILED} failures)")))?;
            let edits = get(p, "edits").ok_or_else(|| bad(CMD, "`editId` needs `edits`"))?;
            apply_edits(old, edits)?
        }
        (Some(_), Some(_)) => return Err(bad(CMD, "pass `source` or `editId` + `edits`, not both")),
        (None, None) => return Err(bad(CMD, "`source` (the script) is required")),
    };
    if source.trim().is_empty() {
        return Err(bad(CMD, "the script is empty"));
    }
    let timeout = match num(p, "timeout") {
        Some(t) if t.is_finite() && t > 0.0 => t.min(MAX_TIMEOUT),
        Some(_) => return Err(bad(CMD, "`timeout` must be a positive number of seconds")),
        None => figcraft_script::Options::default().timeout,
    };
    let args = get(p, "args").cloned().unwrap_or(Value::Null);
    let label = match string(p, "label").map(|l| l.trim().chars().take(80).collect::<String>()) {
        Some(l) if !l.is_empty() => format!("Script: {l}"),
        _ => "Script".to_string(),
    };
    let opts = figcraft_script::Options { args, timeout };
    let before = s.ds()?.doc.clone();
    let started = web_time::Instant::now();
    let mut kept = None;
    let r = s.grouped(&label, |s| {
        let outcome = run_on(s, &source, &opts);
        if outcome.ok() {
            Ok(outcome)
        } else {
            kept = Some(outcome);
            Err(EngineError::Other("the script failed".into()))
        }
    });
    let ms = (started.elapsed().as_secs_f64() * 1000.0).round();
    let outcome = match r {
        Ok(outcome) => outcome,
        Err(e) => match kept {
            // Failed in the script: everything it did was rolled back.
            Some(outcome) => {
                let id = s.scripts.keep(source);
                let mut out = outcome.to_json();
                out["editId"] = json!(id);
                out["rolledBack"] = json!(true);
                out["ms"] = json!(ms);
                return Ok(out);
            }
            None => return Err(e),
        },
    };
    let after = s.ds()?.doc.clone();
    let mut out = outcome.to_json();
    out["changes"] = changes(&before, &after);
    out["ms"] = json!(ms);
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![spec!(
        "script.run",
        "Run script",
        "{source (JavaScript; `return` hands back the result) | editId + edits [{find, replace, all?}] (fix a failed run without resending it), args? (the script's `args`), timeout? (seconds, default 10), label? (undo step: \"Script: <label>\")}  one undo step, rolled back when the script fails → {ok, result, logs, calls, changes {created, createdTop, changed, removed, counts}, ms} or {ok: false, error {message, line, column}, editId, logs}. API: fc.run(command, params), fc.get(id), fc.getNodeById(id), fc.currentPage, fc.pages, fc.selection (settable), fc.findAll(fn), fc.findOne(fn), fc.create(type, params) / fc.createFrame|Rectangle|Ellipse|Text|…(params); layers: node.children, node.parent, any node.get property to read, any node.set property to write (node.fills = […]), findAll, findOne, findChildren, appendChild, insertChild, clone, remove, set(props), toJSON; console.log; args. See docs/scripting.md",
        false,
        run
    )]
}
