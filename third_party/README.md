# Patched dependencies

Copies of crates from crates.io with small FigCraft changes, used through `[patch.crates-io]` in
the workspace `Cargo.toml`. Each keeps its own licence files (vello: Apache-2.0 OR MIT). Every
change is marked `FigCraft patch` in the source.

| Crate | Version | Change |
|---|---|---|
| `vello_encoding` | 0.11.0 | `src/ramp_cache.rs`: gradient ramps in 16 bits (two rows of 8: high bytes, then low bytes) instead of one 8-bit row. |
| `vello_shaders` | 0.11.0 | `shader/fine.wgsl`: reads the two rows (between the two nearest samples) and dithers each output pixel by up to half an 8-bit step. |

Why: in 8 bits a faint gradient (a dark card fill, a soft glow) spans a handful of levels, each
drawn as a flat band tens of pixels wide; browsers dither gradients, so the incumbent doesn't band.

Updating vello: copy the new versions here from `~/.cargo/registry/src/*/`, then reapply the
changes marked `FigCraft patch` (both crates must be the same version as `vello`).
