//! `card.flash`: when Claude edits a comp, its cards flash, and a "Before / After" badge on each
//! holds (pressed) the frame from before the edit. The desktop app keeps each card's frames of the
//! revision before (`effectcraft_board` in the UI); this records which cards flash.

use figcraft_doc::NodeId;
use serde_json::{Value, json};

use crate::cmd::CommandSpec;
use crate::params::{boolean, get, opt_id};
use crate::{Result, Session, bad, spec};

/// Flashes remembered (the newest).
const MAX_FLASHES: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Flash {
    pub serial: u64,
    pub comp: String,
    pub comp_id: u64,
    pub cards: Vec<NodeId>,
    /// Offer the Before / After badge.
    pub before: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Flashes {
    pub serial: u64,
    pub list: Vec<Flash>,
}

impl Flashes {
    /// The newest flash of card `id`.
    pub fn of(&self, id: NodeId) -> Option<&Flash> {
        self.list.iter().rev().find(|f| f.cards.contains(&id))
    }
    /// Forget card `id`'s badge (it was dismissed).
    pub fn dismiss(&mut self, id: NodeId) {
        for f in &mut self.list {
            f.cards.retain(|c| *c != id);
        }
        self.list.retain(|f| !f.cards.is_empty());
    }
}

fn flash(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "card.flash";
    let doc = s.doc()?;
    let (cards, comp, comp_id): (Vec<NodeId>, String, u64) = if let Some(id) = opt_id(cmd, p, "card")? {
        let c = doc.node(id).and_then(|n| n.effectcraft.as_deref()).ok_or_else(|| bad(cmd, "not an EffectCraft card"))?;
        (vec![id], c.comp.clone(), c.comp_id)
    } else {
        let v = get(p, "comp").filter(|v| !v.is_null()).ok_or_else(|| bad(cmd, "pass `comp` (name or id) or `card`"))?;
        let (name, cid) = (v.as_str().map(str::trim).map(str::to_string), v.as_u64());
        if name.as_deref().is_some_and(str::is_empty) || (name.is_none() && cid.is_none()) {
            return Err(bad(cmd, "`comp` is a comp name or id"));
        }
        let project = get(p, "ecproj").or_else(|| get(p, "project")).and_then(Value::as_str);
        let hits: Vec<(NodeId, String, u64)> = doc
            .nodes()
            .filter_map(|n| {
                let c = n.effectcraft.as_deref()?;
                let same = cid == Some(c.comp_id) && c.comp_id > 0 || name.as_deref().is_some_and(|x| x == c.comp);
                (same && project.is_none_or(|pr| pr == c.project)).then(|| (n.id, c.comp.clone(), c.comp_id))
            })
            .take(256)
            .collect();
        let first = hits.first().cloned().ok_or_else(|| bad(cmd, "no card shows that comp"))?;
        (hits.into_iter().map(|h| h.0).collect(), first.1, first.2)
    };
    let before = boolean(p, "before").unwrap_or(true);
    let f = &mut s.ecboard.flash;
    f.serial = f.serial.wrapping_add(1);
    let serial = f.serial;
    f.list.push(Flash { serial, comp: comp.clone(), comp_id, cards: cards.clone(), before });
    if f.list.len() > MAX_FLASHES {
        let extra = f.list.len() - MAX_FLASHES;
        f.list.drain(..extra);
    }
    s.request_ui("card.flash", json!({"serial": serial, "cards": cards, "comp": comp, "compId": comp_id, "before": before}));
    s.ecboard.push("flash", json!({"serial": serial, "cards": cards, "comp": comp, "before": before}));
    Ok(json!({"serial": serial, "cards": cards, "comp": comp, "compId": comp_id, "before": before}))
}

fn dismiss(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "card.flashDismiss";
    let ids: Vec<NodeId> = match opt_id(cmd, p, "card")? {
        Some(id) => vec![id],
        None => s.ecboard.flash.list.iter().flat_map(|f| f.cards.clone()).collect(),
    };
    for id in &ids {
        s.ecboard.flash.dismiss(*id);
    }
    s.request_ui("card.flashDismiss", json!({"cards": ids}));
    Ok(json!({"dismissed": ids}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "card.flash",
            "Flash Edited Card",
            "{comp (name or id) | card, ecproj?, before? (default true: a Before / After badge on the card; held, it shows the frame from before the edit)}  Claude edited this comp: its cards flash → {serial, cards, comp, compId, before}",
            false,
            flash
        ),
        spec!("card.flashDismiss", "Dismiss Before / After", "{card? (default every card)}  the badge goes → {dismissed}", false, dismiss),
    ]
}
