# ADR 0001: `unsafe` Apple media FFI in `crates/video`, and nowhere else

- **Status:** accepted (2026-10-21, approved by the project owner, Elias, for his fork).
- **Mirrors:** EffectCraft's ADR 0001 (platform FFI for hardware ProRes) and FilmCraft's.

## Context

FigCraft is pure Rust and the workspace sets `unsafe_code = "forbid"`. Video cards (a dropped
`.mp4` / `.mov` / `.m4v`) should let a page hold a library of clips and play many of them at once.
Decoding H.264 / HEVC on the CPU costs milliseconds per frame per clip; twenty clips at 30 fps
would take several cores. The Apple media engine decodes them for almost no CPU, and AVFoundation
reads the file, decodes on that engine and scales the frames to the card in one call.

Reaching AVFoundation, CoreMedia and CoreVideo needs FFI, which is `unsafe` Rust. The bindings
(the `objc2-*` crates, MIT / Apache-2.0 / Zlib) are generated declarations; nothing GPL/LGPL is
linked. `objc2`, `objc2-foundation` and `objc2-core-foundation` were already in `Cargo.lock`
through eframe / winit.

## Decision

Allow `unsafe` in exactly one crate, `crates/video` (`figcraft-video`, layer L2), in one module
(`apple.rs`, macOS only), for one job: reading video files for **playback on the canvas** (probe,
frames scaled to the card). Not encoding, not export, not audio, not other OS integration: anything
else needs a new decision.

Containment rules:

- The crate sets `unsafe_code = "deny"` (not `forbid`); only `mod apple` opts in with
  `#[allow(unsafe_code)]`. Every other crate keeps `forbid`.
- Every `unsafe` block has a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks = "deny"`).
- Nothing in it panics (the workspace's no-`unwrap` lints apply); every failure is an `Error`,
  and the card then shows its file name and why.
- Every call runs in its own autorelease pool (the decoders live on worker threads).
- The public API is safe and portable: on other systems it answers `Error::Unsupported` and the
  document, engine, CLI and MCP still place and describe video cards (given their sizes).

## Consequences

- Video plays only in the macOS desktop app for now. Windows (Media Foundation) or a pure-Rust
  decoder would each be a new decision.
- Video files are linked by path, not embedded: a document that travels without its clips shows
  each card's name and that the file is missing.
