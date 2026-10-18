# Scripts

A script is JavaScript run against the open document in one go. It reads layers straight from the
document and changes them through the same commands as the menus, the MCP server and plugins, so
an agent (or anyone with the CLI or control channel) can loop, branch and compute over hundreds of
layers in a single call instead of one command per round trip.

```js
// Every text under 14 px goes to 14 px; report how many changed.
let n = 0;
for (const t of fc.findAll(l => l.type === 'TEXT')) {
  if (t.fontSize < 14) { t.fontSize = 14; n++; }
}
return n;
```

## Running a script

| Surface | How |
|---|---|
| MCP | `run_script {source, args?, timeout?, label?, screenshot?, maxSize?}`, or `{editId, edits}` to fix a failed run |
| Any command surface (CLI `run`, control channel, `run_command`) | the command `script.run` with the same parameters (no screenshot) |

The engine runs scripts with [boa](https://github.com/boa-dev/boa), a JavaScript interpreter written
in Rust (no JIT; it runs in the web build too). Scripts are ES2023+ with no browser or Node APIs:
no `fetch`, no file system, no timers. The script is the body of a function, so `return` hands back
the result.

## What a run is

- **One undo step**, labelled `Script` or `Script: <label>`. Undo takes the whole run back and redo
  brings it back again (handy when a person undoes an agent's script by mistake). A run that
  changes nothing adds no step.
- **All or nothing.** When the script throws (or a command it runs fails and the script doesn't
  catch it), everything it did is rolled back. The result has `ok: false`, the `error` with its
  `line` and `column` in the script, the `logs` so far and an `editId`.
- **Fix with edits.** `script.run {editId, edits: [{find, replace, all?}]}` applies the
  replacements to that failed script in order and runs it again, so the next call carries only the
  fix. Each `find` must occur exactly once unless `all: true`. The last 16 failed scripts of a
  session are kept.
- **Time budget.** `timeout` seconds (default 10, at most 3600), checked on every API call; once it
  runs out every API call fails and no `catch` can keep the run going. A loop that calls no API
  stops after 100 million iterations; recursion is limited to 1000 calls.
- **What it may do** is what commands may do: imports, exports and file commands included. Scripts
  never open dialogs or panels (such a command is refused) and can't run other scripts.

A successful run returns:

```json
{"ok": true, "result": …, "logs": ["…"], "calls": 12, "ms": 31,
 "changes": {"created": [ids], "createdTop": [ids], "changed": [ids], "removed": [ids],
             "counts": {"created": 0, "changed": 0, "removed": 0}}}
```

`createdTop` leaves out layers made inside other new layers. Each list holds at most 1000 ids;
`counts` are exact. The MCP tool's `screenshot: true` renders `createdTop` (else the changed
layers), or the ids given, and returns the PNG with the result, so an agent can look at what it
made and fix it in the next call.

## The API

### `fc`

| Member | |
|---|---|
| `fc.run(command, params?)` | Run any engine command (`list_commands`) and return its result. A refusal throws an `Error` whose message starts with the command id. |
| `fc.get(id)` | A layer as plain JSON (the `node.get` properties plus `parent` and `children` ids), or `null`. |
| `fc.getNodeById(id)` | The layer object, or `null`. |
| `fc.currentPage` | The current page; assign a page (or id) to go to it. |
| `fc.pages` | Every page of the document. |
| `fc.selection` | The selected layers; assign an array of layers or ids to select them (`[]` deselects). |
| `fc.findAll(fn?)`, `fc.findOne(fn)` | Layers on the current page that `fn(layer)` accepts, depth first. |
| `fc.create(type, params?)` | `node.create` a layer and return it. `params` as `node.create` (`x`, `y`, `width`, `height`, `name`, `text`, `props`, `parent` (a layer or id), `index`); a string is a text layer's text. New layers don't take the selection unless `select: true`. |
| `fc.createFrame`, `createRectangle`, `createEllipse`, `createPolygon`, `createStar`, `createLine`, `createArrow`, `createVector`, `createText`, `createSection`, `createComponent`, `createSlice` | `fc.create` with that type. |

### Layers

A layer object stands for one layer id; the same layer is always the same object (`===` works).

- **Read** any property `node.get` reports: `name`, `type` (`FRAME`, `TEXT`, `RECTANGLE`, …, as in Figma), `x`, `y`, `width`, `height`,
  `rotation`, `visible`, `opacity`, `fills`, `strokes`, `effects`, `cornerRadius`, `layout`,
  `characters`, `fontSize`, … (see `inspect_document`). Absent properties read as `undefined`.
  `parent`, `children` (layer objects) and `removed` come from the tree. Reads are cached until
  the script's next write.
- **Write** any `node.set` property by assigning it: `node.fills = ['#3366FF']`, `node.x += 10`,
  `text.characters = 'Hi'`. Each assignment is one `node.set`; `set({…})` writes several at once.
  Values read back are copies: change `node.fills` by assigning a new array.
  `id`, `type`, `parent`, `children`, `removed`, `absoluteBounds` and `childCount` are read-only.
- **Methods**: `findAll(fn?)`, `findOne(fn)`, `findChildren(fn?)`, `appendChild(layer)`,
  `insertChild(index, layer)` (both `node.reparent`), `clone()` (`edit.duplicate`, returns the
  copy), `remove()` (`edit.delete`), `set(props)`, `toJSON()`, `toString()`.

### Other globals

`args` is the caller's `args` (any JSON). `console.log`, `info` and `debug` add a line to `logs`
(non-strings as JSON); `console.warn` and `console.error` prefix theirs. At most 1000 lines or
256 KB are kept.

## Examples

```js
// Rename text layers after their content.
for (const t of fc.findAll(l => l.type === 'TEXT'))
  t.name = t.characters.toLowerCase().replace(/\s+/g, '-').slice(0, 40);
```

```js
// Lay the selection out in a grid (args: {cols: 4, gap: 16}).
fc.selection.forEach((l, i) => {
  l.x = (i % args.cols) * (l.width + args.gap);
  l.y = Math.floor(i / args.cols) * (l.height + args.gap);
});
```

```js
// One card per row of data.
args.rows.forEach((row, i) => {
  const card = fc.createFrame({x: i * 220, width: 200, height: 120, name: row.title,
                               props: {fills: ['#FFFFFF'], cornerRadius: 12}});
  fc.createText({parent: card, x: 16, y: 16, text: row.title, props: {fontSize: 18}});
});
```

```js
// Report fills that aren't on the palette (args: {palette: ['#ff0000', …]}), changing nothing.
return fc.findAll(l => (l.fills || []).some(p => p.type === 'solid' &&
                  !args.palette.includes(p.color.toLowerCase())))
         .map(l => ({id: l.id, name: l.name, color: l.fills[0].color}));
```

## Not (yet) included

- Figma plugin API compatibility: names follow Figma's public docs where FigCraft has the feature,
  under `fc`, but plugins written for Figma don't run unchanged.
- A Scripts panel in the app and saved scripts as menu commands.
- `async` / `await` APIs (everything is synchronous).
