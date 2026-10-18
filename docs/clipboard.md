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
- Limits: 256 MiB of HTML, 16 MiB of metadata, 4,096 images (links past that are left out and
  counted, never the whole copy) and 1 GiB of downloaded image bytes in total (64 MiB for one
  image). Eight images download at a time; each request times out after 30 seconds and no new
  request starts after five minutes. (A storyboard page of about 40 frames named 376 images; at
  the old cap of 256 the whole copy was refused and fell back to the plain design-layer paste,
  which loads no images.) (Until 2026-10-18 images came one at a
  time within 30 seconds and 64 MiB, so a page of photos pasted mostly as grey boxes.)
  The shared `.fig` decoder also enforces its decompression, nesting and node limits.
- Missing, expired or unavailable images keep their own hash and show a hatched missing-image
  placeholder; the import report says how many didn't load and why (timed out, the server's
  answer, over the limit). **File ▸ Relink missing images…** (`image.relinkMissing {path | data}`)
  fills them in from a `.fig` copy of the source file or from a folder of the images (files named
  by their hash, or whose bytes' SHA-1 is the hash). Copy again in Figma to refresh expired links. Invalid clipboard data produces an error, never an
  accidental paste of an older internal selection.
- Main components the copy carries on its hidden page (library copies) come along with the
  instances that use them, which stay instances; colours bound to library variables keep their
  binding (referenced by the variable's key) and the frame's variable mode, so a dark-mode
  frame pastes dark.
- The layers paste at once, each image as a missing-image placeholder under its own hash; the
  images download in the background and fill those placeholders in when they arrive (`image.relinkMissing
  {images}`, its own undo step), then a toast says so, or the import report lists what didn't
  load and why. Editing, switching pages or tabs meanwhile is fine: the images wait for their
  document to be in front again, and are dropped if it closes. (Until 2026-10-18 the paste waited
  for every image and was cancelled when anything changed in the meantime, so a page of 376
  images, a minute or more of downloads, often never pasted.)
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

`edit.pasteFigma {html, images?: {hash: base64}, imageFailures?: {hash: reason}, parent?, x?, y?}` returns `{ids, warnings, missingImages: [{hash, reason}], summary: {layers, images}}`; the first warning is the headline (how many images did not load, and why).
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
