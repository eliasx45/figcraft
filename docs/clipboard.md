# Figma clipboard import

On the desktop, select objects in Figma, Copy (Cmd+C / Ctrl+C), switch to FigCraft and Paste.
No Figma plugin, personal access token or agent session is needed. Edit ▸ Paste and the canvas's
Paste here use the same command. Pasting while editing text keeps the text-editing behavior.

The copied objects become native FigCraft layers through the same Kiwi decoder and REST mapper
as `.fig` file import. The difference is the destination: clipboard import inserts a selection
into the current design, using normal Paste placement, instead of appending pages. It is one
undo step. Copies get fresh node, style, variable and collection IDs; references between copied
roots follow their new IDs. Existing destination resources and the internal clipboard are kept.

## Images and limitations

Figma's HTML clipboard carries a `fig-kiwi` canvas in a base64 `figma` section. The `figmeta`
section can carry `imageHashes`, mapping image hashes to signed Figma CDN URLs. These are
clipboard output observed through ordinary Copy, not application internals or network traffic.
The desktop resolves those images in the background, then supplies their bytes to the same
image resolver the file importer uses. Image data is embedded when the design is saved.

- Only HTTPS image URLs on `s3-alpha-sig.figma.com` and `s3-alpha.figma.com`, with the matching
  `/img/` hash path, are fetched. Redirects are not followed. No Figma credentials are used.
- Limits: 64 MiB of HTML, 1 MiB of metadata, 256 images and 64 MiB of downloaded image bytes
  in total. Individual requests time out after 10 seconds; new requests stop after 30 seconds.
  The shared `.fig` decoder also enforces its decompression, nesting and node limits.
- Missing, expired or unavailable images receive the importer's placeholder and warning. Copy
  again in Figma to refresh expired links. Invalid clipboard data produces an error, never an
  accidental paste of an older internal selection.
- Switching designs/pages/selections or editing while images load cancels that pending paste.
  Each window keeps its own pending operation. A second paste waits until the current one ends.
- Fidelity is the existing best-effort `.fig` import's fidelity, not a separate guarantee.
  Outlined text remains editable vector geometry. Components without their main in the imported
  selection become editable frames with a warning; prototype destinations outside the copy are
  omitted with a warning. Supported auto layout, styles and variables remain structured.
- This implements Figma → FigCraft. The reverse native clipboard format is not written.
  The web build still uses its existing text/image/SVG clipboard path; native rich clipboard
  reading is a desktop service. Windows/Linux behavior needs platform QA.

Closed painted contours without explicit region records are recovered as editable filled
regions by the shared importer. Vector masks use their geometry even when their contours have
no fill regions; the canvas, SVG and PDF share that outline behavior.

## Command and verification

`edit.pasteFigma {html, images?: {hash: base64}, parent?, x?, y?}` returns `{ids, warnings}`.
`x,y` are the pasted selection's top-left in page coordinates. CLI/MCP/control callers supply
image bytes themselves; engine commands never perform network access or open dialogs.

Regression tests construct synthetic documents and clipboard envelopes in code. They cover
editable shapes/text, normal placement, cross-root links, ID collisions, variable aliases and
component sources, rollback, undo/redo and save/reopen, rich-data precedence, stale destinations,
and the image URL allowlist. Private reference copies stay in gitignored `plan/` and are never
shipped as fixtures.

Manual verification (2026-10-14): normal Copy from Figma in Chrome on macOS and the desktop
Paste path were exercised with a 108-object vector selection, editable text plus an image, and
a single nested rectangle. The image was checked after saving as embedded bytes. Matched-size
1212×682 output for the vector example was compared visually and numerically against Figma
(mean absolute RGB-channel error 0.00211 on a 0–1 scale). This one local sample is not a
guarantee for arbitrary Figma files. Reference artwork and captures remain local.
