//! `canvas.presence.*`: agents shown on the board, pen.dev style (Elias, 28 Oct 2026: "if an agent
//! is working on its comp, its comp is glowing like this and has this system that Pen Dev has.
//! include that too").
//!
//! The host (Ghostex) tells the board which card each agent works on and what it touched, from
//! the agent's real EffectCraft command targets: `canvas.presence.set {agent, name, color,
//! card|comp, layer?, bbox?, status}`. The desktop app draws it (`effectcraft_presence` in the UI):
//! the card tinted and outlined in the agent's colour (pulsing while working, steady when done), an
//! arrow cursor with a name tag at the touched layer's box, a thin box round it that fades. Nothing
//! here is made up: no `bbox`, no box, and the cursor sits at the card's corner.
//!
//! Session state only, never the document. Hovering or clicking an agent on the canvas is a
//! `presence.hover` / `presence.open` event in `board.events`.

use figcraft_doc::NodeId;
use serde_json::{Value, json};

use crate::cmd::CommandSpec;
use crate::ecboard::{KIND, text_param};
use crate::params::{color, get, opt_id};
use crate::{Result, Session, bad, spec};

/// The most agents shown at once.
pub const MAX_AGENTS: usize = 16;
/// The longest agent id, name and layer kept (characters).
const MAX_ID: usize = 64;
const MAX_NAME: usize = 48;
const MAX_LAYER: usize = 128;
/// Comp pixels beyond this are junk.
const MAX_PX: f64 = 1.0e6;

/// Colours for agents that don't name one (picked by the agent id, so it keeps its colour).
pub const PALETTE: [[u8; 3]; 8] = [
    [0xFF, 0xB0, 0x20],
    [0x2E, 0xC4, 0xB6],
    [0xFF, 0x5C, 0x8A],
    [0x2D, 0x8C, 0xFF],
    [0x3D, 0xDC, 0x84],
    [0xFF, 0xD6, 0x0A],
    [0xFF, 0x6B, 0x3D],
    [0x9A, 0xE6, 0x3A],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The card pulses.
    Working,
    /// The card holds a steady glow: "changed".
    Done,
    /// Only the cursor, the card faintly outlined.
    Idle,
}

impl Status {
    pub fn id(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Done => "done",
            Status::Idle => "idle",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "working" | "busy" => Some(Status::Working),
            "done" | "changed" => Some(Status::Done),
            "idle" => Some(Status::Idle),
            _ => None,
        }
    }
}

/// One agent on the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub agent: String,
    pub name: String,
    pub color: [u8; 3],
    pub card: NodeId,
    pub comp: String,
    pub comp_id: u64,
    /// The touched layer (name or id, as the host gave it).
    pub layer: Option<String>,
    /// The touched layer's box in the card comp's pixels: x, y, width, height.
    pub bbox: Option<[f64; 4]>,
    pub status: Status,
    /// Bumped by every `canvas.presence.set` of this agent (the UI glides the cursor and shows the
    /// touch box from it).
    pub serial: u64,
}

impl Agent {
    pub fn color_hex(&self) -> String {
        let [r, g, b] = self.color;
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// The name tag: the name, then the card's scene code ("A2 · S1 06") unless the name has one.
    pub fn tag(&self) -> String {
        let code: Vec<&str> = self.comp.split_whitespace().take(2).collect();
        if self.name.contains('·') || code.is_empty() { self.name.clone() } else { format!("{} · {}", self.name, code.join(" ")) }
    }

    pub fn json(&self) -> Value {
        json!({"agent": self.agent, "name": self.name, "tag": self.tag(), "color": self.color_hex(), "card": self.card,
            "comp": self.comp, "compId": self.comp_id, "layer": self.layer, "bbox": self.bbox, "status": self.status.id(), "serial": self.serial})
    }
}

#[derive(Clone, Debug, Default)]
pub struct Presence {
    pub serial: u64,
    pub list: Vec<Agent>,
}

impl Presence {
    pub fn of(&self, agent: &str) -> Option<&Agent> {
        self.list.iter().find(|a| a.agent == agent)
    }
}

/// A stable palette colour for agent id `agent`.
pub fn color_for(agent: &str) -> [u8; 3] {
    // FNV-1a: the same id gets the same colour in every session.
    let h = agent.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    PALETTE.get((h % PALETTE.len() as u64) as usize).copied().unwrap_or([0xFF, 0xB0, 0x20])
}

/// `[x, y, w, h]` or `{x, y, w|width, h|height}` in comp pixels, finite, w and h ≥ 0.
pub fn bbox_of(v: &Value) -> Option<[f64; 4]> {
    let n = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|x| x.is_finite() && x.abs() <= MAX_PX);
    let b = match v {
        Value::Array(a) if a.len() == 4 => [n(a.first())?, n(a.get(1))?, n(a.get(2))?, n(a.get(3))?],
        Value::Object(o) => [n(o.get("x"))?, n(o.get("y"))?, n(o.get("w").or(o.get("width")))?, n(o.get("h").or(o.get("height")))?],
        _ => return None,
    };
    (b[2] >= 0.0 && b[3] >= 0.0).then_some(b)
}

/// The card agent `agent` works on: `card`, else a card showing `comp` (name or id; of `ecproj`
/// when given), the agent's current card first, then a MAIN board card, then any.
fn card_for(cmd: &str, s: &Session, p: &Value, agent: &str) -> Result<(NodeId, String, u64)> {
    let doc = s.doc()?;
    if let Some(id) = opt_id(cmd, p, "card")? {
        let c = doc.node(id).and_then(|n| n.effectcraft.as_deref()).ok_or_else(|| bad(cmd, "`card` is not an EffectCraft card"))?;
        return Ok((id, c.comp.clone(), c.comp_id));
    }
    let v = get(p, "comp").filter(|v| !v.is_null()).ok_or_else(|| bad(cmd, "pass `card` or `comp` (name or id)"))?;
    let (name, cid) = (v.as_str().map(str::trim).map(str::to_string), v.as_u64());
    if name.as_deref().is_some_and(str::is_empty) || (name.is_none() && cid.is_none()) {
        return Err(bad(cmd, "`comp` is a comp name or id"));
    }
    let project = get(p, "ecproj").or_else(|| get(p, "project")).and_then(Value::as_str);
    let current = s.ecboard.presence.of(agent).map(|a| a.card);
    let mut best: Option<(u8, NodeId, String, u64)> = None;
    for n in doc.nodes().take(100_000) {
        let Some(c) = n.effectcraft.as_deref() else { continue };
        let same = (cid.is_some_and(|i| i == c.comp_id) && c.comp_id > 0) || name.as_deref().is_some_and(|x| x == c.comp);
        if !same || project.is_some_and(|pr| pr != c.project) {
            continue;
        }
        let rank = if current == Some(n.id) {
            0
        } else if n.plugin_data.get(KIND).is_some_and(|k| k == "card") {
            1
        } else {
            2
        };
        if best.as_ref().is_none_or(|b| rank < b.0) {
            best = Some((rank, n.id, c.comp.clone(), c.comp_id));
        }
    }
    best.map(|b| (b.1, b.2, b.3)).ok_or_else(|| bad(cmd, "no card shows that comp"))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.presence.set";
    let agent = text_param(p, "agent", MAX_ID);
    if agent.is_empty() {
        return Err(bad(cmd, "`agent` (an id, like \"A2\") is required"));
    }
    let name = Some(text_param(p, "name", MAX_NAME)).filter(|n| !n.is_empty()).unwrap_or_else(|| agent.clone());
    let rgb = match get(p, "color").filter(|v| !v.is_null()) {
        Some(_) => {
            let c = color(p, "color").ok_or_else(|| bad(cmd, "`color` is \"#RRGGBB\""))?.to_rgba8();
            [c[0], c[1], c[2]]
        }
        None => s.ecboard.presence.of(&agent).map_or_else(|| color_for(&agent), |a| a.color),
    };
    let status = match get(p, "status").filter(|v| !v.is_null()) {
        Some(v) => v.as_str().and_then(Status::parse).ok_or_else(|| bad(cmd, "`status` is working, done or idle"))?,
        None => Status::Working,
    };
    let bbox = match get(p, "bbox").filter(|v| !v.is_null()) {
        Some(v) => Some(bbox_of(v).ok_or_else(|| bad(cmd, "`bbox` is [x, y, width, height] in comp pixels"))?),
        None => None,
    };
    let layer = match get(p, "layer") {
        Some(Value::String(l)) => Some(l.trim().chars().take(MAX_LAYER).collect::<String>()).filter(|l| !l.is_empty()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let (card, comp, comp_id) = card_for(cmd, s, p, &agent)?;
    let pr = &mut s.ecboard.presence;
    if pr.of(&agent).is_none() && pr.list.len() >= MAX_AGENTS {
        return Err(bad(cmd, "at most 16 agents at once (canvas.presence.clear one first)"));
    }
    pr.serial = pr.serial.wrapping_add(1);
    let a = Agent { agent: agent.clone(), name, color: rgb, card, comp, comp_id, layer, bbox, status, serial: pr.serial };
    let out = a.json();
    match pr.list.iter_mut().find(|x| x.agent == agent) {
        Some(x) => *x = a,
        None => pr.list.push(a),
    }
    Ok(out)
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    let agent = text_param(p, "agent", MAX_ID);
    let pr = &mut s.ecboard.presence;
    let cleared: Vec<String> = pr.list.iter().filter(|a| agent.is_empty() || a.agent == agent).map(|a| a.agent.clone()).collect();
    pr.list.retain(|a| !cleared.contains(&a.agent));
    pr.serial = pr.serial.wrapping_add(1);
    Ok(json!({"cleared": cleared}))
}

fn state(s: &mut Session, _p: &Value) -> Result<Value> {
    let pr = &s.ecboard.presence;
    Ok(json!({"serial": pr.serial, "agents": pr.list.iter().map(Agent::json).collect::<Vec<_>>()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "canvas.presence.set",
            "Show Agent on Board",
            "{agent (id, e.g. \"A2\"), name? (default the id), color? (\"#RRGGBB\"; default a palette colour kept per agent), card? | comp? (name or id; with ecproj?), layer? (name or id), bbox? ([x, y, width, height] in the comp's pixels: the touched layer), status? (working (default) | done | idle)}  shows an agent working on a card, pen.dev style: the card tinted and outlined in its colour (pulsing while working, a steady glow when done), an arrow cursor with a name tag at the bbox gliding there, a thin box round the bbox fading after 1.2 s. Session state, never the document; set again to move or change it → {agent, name, tag, color, card, comp, compId, layer, bbox, status, serial}",
            false,
            set
        ),
        spec!("canvas.presence.clear", "Hide Agents on Board", "{agent? (default every agent)}  → {cleared: [agent ids]}", false, clear),
        spec!(
            "canvas.presence.state",
            "Agents on Board",
            "{}  → {serial, agents: [{agent, name, tag, color, card, comp, compId, layer, bbox, status, serial}]}; hovering an agent's cursor or glow is a presence.hover {agent} event in board.events (agent null when the pointer leaves), a click presence.open {agent}",
            false,
            state
        ),
    ]
}
