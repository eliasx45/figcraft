//! Commands the host answers itself on FigCraft's control channel (Ghostex's `editing.saveAll`:
//! the board and the EffectCraft project in one go). Claude reaches them the way it reaches any
//! FigCraft command (`engine.execute {command}`, or the method by name); the frame takes them out
//! of the queue before FigCraft sees them and hands them to the host, which answers each.

use figcraft_ui_egui::{ControlReply, ControlRequest};
use serde_json::{Value, json};

/// A request for a command the host claimed ([`crate::FigEmbedded::claim_commands`]). Dropped
/// unanswered, the caller gets an error.
pub struct HostRequest {
    pub command: String,
    pub params: Value,
    request: Option<ControlRequest>,
}

impl HostRequest {
    pub fn reply(mut self, result: Result<Value, String>) {
        if let Some(r) = self.request.take() {
            let _ = r.reply.send(ControlReply { id: r.id, result });
        }
    }
}

impl Drop for HostRequest {
    fn drop(&mut self) {
        if let Some(r) = self.request.take() {
            let _ = r.reply.send(ControlReply { id: r.id, result: Err(format!("{} wasn't answered by the host", self.command)) });
        }
    }
}

impl std::fmt::Debug for HostRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostRequest").field("command", &self.command).field("params", &self.params).finish()
    }
}

/// The command a request asks for and its params: `engine.execute {command, params}` or the
/// method by name.
fn command_of(r: &ControlRequest) -> (String, Value) {
    if r.method == "engine.execute" {
        let command = r.params.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = r.params.get("params").cloned().filter(|p| !p.is_null()).unwrap_or_else(|| json!({}));
        (command, params)
    } else {
        let params = if r.params.is_null() { json!({}) } else { r.params.clone() };
        (r.method.clone(), params)
    }
}

/// Takes the requests for `claimed` commands out of `queue` (the rest stay, in order).
pub(crate) fn take(queue: &mut Vec<ControlRequest>, claimed: &[String]) -> Vec<HostRequest> {
    if claimed.is_empty() || queue.is_empty() {
        return Vec::new();
    }
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(queue.len());
    for r in queue.drain(..) {
        let (command, params) = command_of(&r);
        if !r.is_cancelled() && claimed.iter().any(|c| *c == command) {
            taken.push(HostRequest { command, params, request: Some(r) });
        } else {
            kept.push(r);
        }
    }
    *queue = kept;
    taken
}
