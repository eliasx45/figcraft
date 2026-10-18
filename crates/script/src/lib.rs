//! FigCraft scripts: JavaScript run against the open document in one go, so an agent (or a person)
//! can loop, branch and compute over hundreds of layers in a single call instead of one command per
//! round trip. The engine (crate `figcraft-engine`, command `script.run`) decides the policy: one
//! undo step per run, rolled back as a whole when the script fails, which commands a script may
//! call. This crate only runs the script: a pure-Rust interpreter (boa, no JIT, also in the web
//! build) with a time budget, and two layers of API over the engine's commands:
//!
//! - `fc.run(command, params)` runs any engine command and returns its result, `fc.get(id)` reads
//!   a layer as JSON, `args` holds the caller's input and `console.log` collects output lines;
//! - a layer object model with the names designers know: `fc.currentPage`, `fc.selection`,
//!   `fc.getNodeById(id)`, `node.children`, `node.parent`, any property the engine reports
//!   (`node.name`, `node.x`, `node.fills`, …) to read, any `node.set` property to write
//!   (`node.fills = [...]`), and `findAll`, `findOne`, `appendChild`, `clone`, `remove`, ….
//!
//! Reads go straight to the document; every write is an engine command, so validation, live
//! session sync and undo stay exactly as they are for the menus, MCP and plugins. See
//! `docs/scripting.md`.

mod model;
mod runtime;

pub use runtime::{Host, Options, Outcome, ScriptError, run};

#[cfg(test)]
mod tests;
