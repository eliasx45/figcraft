//! Fork-only (`effectcraft-inproc`): EffectCraft's own Pre-compose, Interpret Footage and Replace
//! Footage dialogs for the comp a card's Timeline edits, shown in FigCraft windows.
//!
//! **Entry point (the card's right-click menu, keys, the Timeline's and Project tab's menus):**
//!
//! ```ignore
//! crate::effectcraft_ui::dialogs::request(app: &mut FigApp, card: Option<NodeId>, which: Which)
//!     -> Result<(), String>
//! // Which::{Precompose, InterpretFootage, ReplaceFootage}
//! ```
//!
//! `card`: the card the menu was opened on; its Timeline opens first when it isn't the open one
//! (`None`: the open Timeline). The dialog acts on that Timeline's selection (layers for
//! Pre-compose; for the footage dialogs the Project tab's selected footage, else the footage of
//! the first selected footage layer). Errors are for a toast ("select the layers to pre-compose").
//!
//! The same dialogs come up when EffectCraft's own commands run in the hosted panels without their
//! parameters, as in EffectCraft's app: `layer.precompose` without `name` (⌘⇧C, Layer ▸
//! Pre-compose…, the Timeline's context menu), `file.interpretFootage` / `file.interpretProxy`
//! without settings (⌘⌥G, the Project tab's menu and Interpret button), `file.replaceFootage`
//! without `path` (⌘H, the Project tab's menu). With their parameters they run at once, so
//! programmatic calls never open a dialog, and nothing on the control channel reaches them.
//!
//! The dialog code is EffectCraft's (EMBED10: `panels::precomp::{draft_for, draw}`,
//! `panels::forms::{build_form, draw_form}`); OK runs the command through the Timeline's host, so
//! it is one undo step there (⌘Z / ⌘S in the Timeline, the app's history when it mirrors the
//! EffectCraft app) and the card re-renders. After Pre-compose with Open New Composition ticked,
//! the new comp becomes a card right of the Timeline's card with the Timeline on it (as
//! `effectcraft.newComp` does); untick it and the precomp is still there for the card's explode.

use figcraft_doc::NodeId;

use crate::FigApp;

/// One of the hosted EffectCraft dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    /// Layer ▸ Pre-compose… (⌘⇧C).
    Precompose,
    /// File ▸ Interpret Footage ▸ Main… (⌘⌥G).
    InterpretFootage,
    /// File ▸ Replace Footage ▸ File… (⌘H): a file dialog, then EffectCraft's replace.
    ReplaceFootage,
}

/// Open dialog `which` for card `card`'s Timeline (the open one when `None`).
pub(crate) fn request(app: &mut FigApp, card: Option<NodeId>, which: Which) -> Result<(), String> {
    let _ = (app, card, which);
    Err("not wired yet".into())
}
