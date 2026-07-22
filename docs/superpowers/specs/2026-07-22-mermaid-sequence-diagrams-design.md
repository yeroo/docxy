# Mermaid sequence diagrams — design (Phase 2 of "support all architecture diagrams")

**Goal:** Render ` ```mermaid ` **`sequenceDiagram`** blocks as editable Word
`.docx` shapes AND in the docxy VS Code webview, from one shared geometry — so
the sequence diagrams in real architecture docs stop rendering as a flowchart
approximation and become proper lifeline/message diagrams. No new dependencies.

**Basis:** conversational request (2026-07-22), grounded in the four
`sequenceDiagram` blocks of the Aliaksei VDI doc
(`ELAB/Aliaksei/docs/superpowers/specs/2026-07-20-vdi-domain-model-design.md`).
Their construct set is tight and defines this slice's scope: aliased
participants, `->>` messages (incl. self-messages), `alt/else/end` frames, and
`Note over`. Builds on the merged mermaid engine; stacked on the flowchart-gaps
branch (PR #32).

**Relationship to prior slices:** the flowchart engine is **untouched**. A
`sequenceDiagram` header routes to a new, parallel path. Same architecture
principle as before: one geometry, two renderers (Word DrawingML + webview SVG),
so Word == webview by construction.

## Background — current state and target

Today `sequenceDiagram` falls into the flowchart parser (participants become
nodes, messages become edges) — a readable but wrong approximation (no time axis,
`alt/else` guards mishandled). Target: a real sequence renderer.

Construct scope (covers all four Aliaksei diagrams):
- **Participants** — `participant ID as Label` (and bare `participant ID`);
  drawn as a header box per participant across the top, each with a vertical
  **dashed lifeline** running the full height.
- **Messages** — `A->>B: text`, ordered top-to-bottom, one **row** each: a
  horizontal arrow from A's lifeline to B's with the text as a label above it.
  `->>` = solid line, open arrowhead; `-->>` = dashed line (cheap to include).
- **Self-messages** — `A->>A: text`: a small right-going loop on A's own lifeline
  with the label.
- **Frames** — `alt <cond> … else <cond> … end`: a labeled rectangle spanning the
  participants involved and the vertical range of the enclosed messages, a
  condition tab at top-left, and an `[else <cond>]` divider line between branches.
- **Notes** — `Note over A,B: text` (and `Note over A`): a note box spanning from
  A's to B's lifeline at that row.

Out of scope (not used in the source docs): activation bars, `loop`/`opt`/`par`/
`critical`/`break`, `autonumber`, actor stick-figures, `->`/`--)` variants beyond
solid/dashed.

## Architecture — dispatch + a parallel module

```
```mermaid fence → docxcore
   parse the header line:
     "sequenceDiagram"  ─► crate::mermaid_seq  (NEW module)
     else               ─► crate::mermaid      (existing flowchart engine, untouched)

crate::mermaid_seq:
   parse (participants, messages, frames, notes)
     → layout (participant columns; message rows; frame vertical ranges; note spans)
       → SequenceGeometry
         ├──► emit_drawing()  → <w:drawing> DrawingML (boxes, dashed lifelines,
         │                       custGeom arrows, frame rects, note rects, labels)
         └──► to_json()       → view_json geometry  (kind:"sequence")
                                  → webview buildSequenceSvg() → inline SVG
```

The public entry points stay the same shape as flowcharts so callers don't care
which kind it is:
- `mermaid::to_drawing(src)` and `mermaid::geometry(src)` inspect the header and
  delegate to `mermaid_seq` when it's a sequence diagram (or the dispatch lives in
  the callers — decided in the plan; simplest is a thin dispatch inside the
  existing `mermaid::to_drawing`/`geometry`/`source_of` which already own the
  `mermaid:` source marker, so the `descr` round-trip carrier is unchanged).
- `source_of` / the `descr` embedding is shared and unchanged — a sequence diagram
  round-trips md↔docx exactly like a flowchart (source is the carrier).

The webview receives, per diagram, a geometry object tagged `kind`; `buildMermaidSvg`
dispatches: `kind:"sequence"` → `buildSequenceSvg(geo)`, else the existing
flowchart SVG. The `view_json` `mermaid` array and its cell-anchoring
(`MermaidBox`) are reused as-is — a sequence diagram is just another mermaid box
whose geometry has a different shape.

## Components

### 1. `docxcore/src/mermaid_seq.rs` — parser + layout + geometry + DrawingML

**Model:**
```
struct Participant { id: String, label: String, x: i64, w: i64 }   // column center via x + w/2
enum MsgKind { Solid, Dashed }                                      // ->>  vs -->>
struct Message { from: usize, to: usize, text: String, kind: MsgKind, row: usize, self_msg: bool }
struct Frame { label: String, else_label: Option<String>, span_first: usize, span_last: usize,
               row_start: usize, else_row: Option<usize>, row_end: usize }   // participant + row ranges
struct Note { span_first: usize, span_last: usize, text: String, row: usize }
struct SequenceDiagram { participants, messages, frames, notes }
```

**Parse** (line-oriented, total):
- `participant ID as Label` / `participant ID` / `actor ID …` → a Participant
  (dedup by id). A message referencing an unknown id auto-creates a participant.
- `A->>B: text` / `A-->>B: text` → a Message (kind by arrow; `self_msg = a==b`).
  Split the arrow token to get from/to and the `: text` label. Assign `row` in
  encounter order (each message and each note advances the row counter).
- `alt cond` pushes a frame-open (record `span`/`row_start`); `else cond` sets
  `else_label`/`else_row`; `end` closes the innermost open frame (record
  `row_end`, and `span_first/last` = min/max participant column touched by the
  frame's messages). A stray `end`/`else` with no open frame is ignored.
- `Note over A,B: text` / `Note over A: text` → a Note at the current row (spanning
  the named participants); advances the row.
- Comments (`%%`), `title`, `autonumber`, and unknown directives are skipped.

**Layout** (EMU, reuse `mermaid`'s EMU constants where sensible):
- Participants: fixed column width from the widest of {label, its messages'
  midpoint text}, min/max clamped; columns laid left-to-right with a gap; each
  participant's lifeline x = column center.
- Rows: header band at top (participant boxes); each message/note row is a fixed
  vertical step; total height = header + rows·step + margins. A self-message row
  gets extra height for its loop.
- Frames: rect from `span_first` column-left to `span_last` column-right
  (padded), y from `row_start` to `row_end` (padded), title tab at top-left;
  `else_row` → a horizontal divider.
- Notes: rect spanning the named participants at the note's row.

**`SequenceGeometry` + `to_json`** — a serializable snapshot the two renderers
share: `{ kind:"sequence", canvasW, canvasH, participants:[{x,y,w,h,label}],
lifelines:[{x,y1,y2}], messages:[{x1,y1,x2,y2,text,dashed,self:bool}],
frames:[{x,y,w,h,label,elseLabel,elseY}], notes:[{x,y,w,h,text}] }`. Hand-rolled
JSON like the flowchart `DiagramGeometry::to_json`.

**`emit_drawing`** — a `<w:drawing>` `wpg` group (same wrapper/namespaces as the
flowchart emitter — factor the shared wrapper if easy, else mirror it): participant
boxes (`rect`), lifelines (thin dashed `cxnSp`/line via `prstDash`), messages
(custGeom arrow polylines with `tailEnd`; dashed for `-->>`; self-message = a
small 3-segment loop), frame rects (`roundRect`, light fill, title run) drawn
BEHIND messages, note rects (a distinct fill). Message/frame/note labels as text
runs. The Mermaid source is embedded in `descr` exactly as the flowchart emitter
does (shared `MARKER`).

### 2. `docxwasm` / bridge — geometry passthrough

`mermaid::geometry(src)` returns JSON already tagged with `kind`. The bridge's
`view_json` `mermaid` array is unchanged — it just serializes whatever
`geometry()` returns. (If `geometry()` currently returns the flowchart struct
type, the dispatch returns the sequence JSON string through the same channel; the
plan picks the exact seam so both kinds flow through one `mermaid` array entry.)

### 3. `offxy-vscode/media/webview.js` — `buildSequenceSvg`

`buildMermaidSvg(geo)` branches on `geo.kind`: `"sequence"` →
`buildSequenceSvg(geo)`. It draws: participant `<rect>` + label (top band);
dashed lifelines (`<line stroke-dasharray>`); messages as `<polyline>` +
arrowhead marker (dashed when `dashed`), self-messages as a small loop path,
label `<text>` above the arrow; frame `<rect>` + title + `[else]` divider
(behind messages); note `<rect>` + text. Escaped text throughout. Same cell→px
overlay path as flowchart mermaid boxes.

## Error handling
- Parsing is total: an unbalanced `alt`/`end`, an unknown participant, or a
  malformed message never panics — it degrades (auto-create participant, ignore
  the stray token, drop a message that can't resolve two endpoints).
- An empty diagram → empty geometry → the webview falls back to the label box
  (same guard as flowcharts: `nodes.length`/participants length 0).

## Testing
- **`docxcore` (unit in `mermaid_seq.rs`):** participants parsed with aliases;
  a message row-orders correctly; `self_msg` detected; `alt/else/end` produces a
  frame with the right participant span + row range; `Note over A,B` spans both;
  dashed vs solid kind; layout gives non-overlapping columns and monotonic rows;
  `geometry()` totality on malformed input; `to_json` well-formed with
  `kind:"sequence"`. **Dispatch:** `mermaid::to_drawing`/`geometry` on a
  `sequenceDiagram` source routes to the sequence path (asserts a lifeline /
  `kind:"sequence"` present) while a `flowchart` source is unchanged.
- **Real-doc regression:** convert the committed
  `offxy-vscode/samples/mermaid/12-aliaksei-provisioning-seq.md` and assert the
  drawing has one box per participant (6), a lifeline per participant, the `alt`
  frame present, and no flowchart-style node-per-message blob.
- **`docxwasm`:** `view_json` on a sequence diagram emits a `mermaid` entry whose
  `geo.kind == "sequence"`.
- **Webview (`mermaid-svg.test.mjs`):** a sequence geometry fixture yields the
  expected participant rects, lifelines, message polylines, a frame rect, and a
  note rect.
- **Gates:** `docxcore`/`docxwasm` tests + fmt/clippy; wasm rebuild;
  `typecheck`/`build`/`test:md-roundtrip`/`test:grid-layout`/`test:mermaid-svg`/
  `test:mcp-parity` (56/56). No version bump; no agent/ctl/MCP change.

## Out of scope
- Activation bars, `loop`/`opt`/`par`/`critical`/`break`, `autonumber`, actor
  stick-figures, numbered messages, `box` participant grouping.
- Non-`->>`/`-->>` arrow variants; message-to-self nesting subtleties beyond a
  single loop; notes with `left of`/`right of` (only `over` this slice).
- Any change to the flowchart engine, the agent/ctl/MCP surface, or the version.
