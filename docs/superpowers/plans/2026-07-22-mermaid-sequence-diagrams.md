# Mermaid Sequence Diagrams Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render ` ```mermaid ` `sequenceDiagram` blocks as proper editable Word `.docx` shapes AND in the docxy webview — participants + lifelines, ordered `->>`/`-->>` messages, self-messages, `alt/else/end` frames, and `Note over` — from one shared geometry so Word == webview.

**Architecture:** A new `docxcore/src/mermaid_seq.rs` module owns sequence parse → layout → `SequenceGeometry` → DrawingML + JSON. A `sequenceDiagram` header routes to it; the flowchart engine (`mermaid.rs`) is otherwise untouched. The webview `buildMermaidSvg` branches on `geo.kind`. The Mermaid source is embedded in `descr` (shared marker), so md↔docx round-trip is unchanged.

**Tech Stack:** Rust std-only `docxcore`; `docxwasm` view_json passthrough; `offxy-vscode/media/webview.js` + `media/mermaid-svg.test.mjs`.

## Global Constraints

- `docxcore` std-only, ZERO external dependencies.
- No version bump (`offxy-vscode/package.json` stays 0.3.0). No agent/ctl/MCP change: `test:mcp-parity` stays **56/56**.
- md↔docx round-trip stays green (source is the carrier, shared `mermaid::MARKER`/`source_of`/`descr`).
- **Shared-geometry invariant:** Word DrawingML and the webview SVG both consume the one `SequenceGeometry` — they must render the same diagram.
- **Flowchart engine untouched:** a `flowchart`/`graph` source must produce byte-identical output to today; only additive dispatch + shared-helper visibility changes touch `mermaid.rs`.
- Scope limits (NOT defects): only participants, `->>`/`-->>` messages, self-messages, `alt/else/end`, `Note over`. No activations/loop/opt/par/autonumber/actors (defer).
- Windows cargo env (bash): `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command; never pipe an exit-code command through `tail`.

## Current seams (context for all tasks)

- `mermaid::to_drawing(src) -> (String, Vec<String>)` (mermaid.rs:165) — DrawingML xml + caption text lines; called at markdown→docx time.
- `mermaid::geometry(src) -> DiagramGeometry` (mermaid.rs:1458); `DiagramGeometry` has `canvas_w`,`canvas_h` + `to_json()`.
- `mermaid::source_of(raw)` (mermaid.rs:181) + `const MARKER` — the `descr` round-trip carrier (shared, unchanged).
- `render.rs:1588-1599` (SmartArt arm) does: `let geo = mermaid::geometry(&src); let (cols,rows)=mermaid_box_cells(geo.canvas_w, geo.canvas_h, width); … geometry_json: geo.to_json()`.
- `lib.rs` (~line 33) has the `pub mod …` list.
- Flowchart emitter's DrawingML wrapper (`mc:AlternateContent`/`wpg:wgp` group) is in `mermaid::emit_drawing`; escaping helpers `xml_escape_text`/`xml_escape_attr`/`escape_source` and EMU constants live in `mermaid.rs`.

---

### Task 1: Sequence module — parser + model

Create the module and parse a `sequenceDiagram` source into a structured model. No layout/emit yet.

**Files:** Create `docxcore/src/mermaid_seq.rs`; Modify `docxcore/src/lib.rs` (add `pub mod mermaid_seq;`)

**Interfaces:**
- Produces (later tasks rely on these exact names):
  - `struct Participant { id: String, label: String }` (+ layout fields `x: i64, w: i64` filled in Task 2 — declare them now, default 0).
  - `enum MsgKind { Solid, Dashed }`
  - `struct Message { from: usize, to: usize, text: String, kind: MsgKind, self_msg: bool, row: usize }`
  - `struct Frame { label: String, else_label: Option<String>, span_first: usize, span_last: usize, row_start: usize, else_row: Option<usize>, row_end: usize }`
  - `struct Note { span_first: usize, span_last: usize, text: String, row: usize }`
  - `struct SequenceDiagram { participants: Vec<Participant>, messages: Vec<Message>, frames: Vec<Frame>, notes: Vec<Note>, rows: usize }`
  - `fn parse(src: &str) -> SequenceDiagram`
  - `pub fn is_sequence(src: &str) -> bool` — the first non-empty, non-`%%` line, lowercased & trimmed, equals `"sequencediagram"`.

- [ ] **Step 1: Write failing tests.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sequence_header() {
        assert!(is_sequence("sequenceDiagram\nA->>B: hi"));
        assert!(is_sequence("%% c\n  sequenceDiagram\n"));
        assert!(!is_sequence("flowchart TD\nA-->B"));
    }

    #[test]
    fn participants_with_alias() {
        let d = parse("sequenceDiagram\nparticipant U as User / AI\nparticipant DL as Desktop\nU->>DL: go");
        assert_eq!(d.participants.len(), 2);
        assert_eq!(d.participants[0].id, "U");
        assert_eq!(d.participants[0].label, "User / AI");
    }

    #[test]
    fn messages_row_ordered_and_self() {
        let d = parse("sequenceDiagram\nA->>B: m1\nB->>B: self\nA-->>B: m2");
        assert_eq!(d.messages.len(), 3);
        assert_eq!(d.messages[0].row, 0);
        assert_eq!(d.messages[1].row, 1);
        assert!(d.messages[1].self_msg);
        assert_eq!(d.messages[2].kind, MsgKind::Dashed);
        // Unknown participants auto-created in first-seen order: A, B.
        assert_eq!(d.participants.len(), 2);
    }

    #[test]
    fn alt_else_end_frame() {
        let d = parse("sequenceDiagram\nA->>B: x\nalt cond1\n  A->>B: y\nelse cond2\n  B->>A: z\nend");
        assert_eq!(d.frames.len(), 1);
        let f = &d.frames[0];
        assert_eq!(f.label, "cond1");
        assert_eq!(f.else_label.as_deref(), Some("cond2"));
        assert!(f.row_start <= f.else_row.unwrap() && f.else_row.unwrap() <= f.row_end);
        assert_eq!(f.span_first, 0); // A
        assert_eq!(f.span_last, 1);  // B
    }

    #[test]
    fn note_over_span() {
        let d = parse("sequenceDiagram\nparticipant A\nparticipant B\nNote over A,B: hello");
        assert_eq!(d.notes.len(), 1);
        assert_eq!(d.notes[0].span_first, 0);
        assert_eq!(d.notes[0].span_last, 1);
        assert_eq!(d.notes[0].text, "hello");
    }

    #[test]
    fn parse_is_total_on_garbage() {
        let _ = parse("sequenceDiagram\nend\nelse\n->> :\nNote over ZZ");
    }
}
```

- [ ] **Step 2: Run — expect FAIL (module/functions undefined).** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_seq`

- [ ] **Step 3: Implement `parse` + `is_sequence`.** Line-oriented, total. Sketch:
- Maintain `participants: Vec<Participant>`, `index: HashMap<String, usize>`, a `get(id) -> usize` that auto-creates a bare participant (label = id) on first sight.
- `row` counter starts 0; each Message and each Note takes the current row then increments it.
- A frame stack `Vec<usize>` (indices into `frames`). `alt <cond>` pushes a `Frame { label: cond, else_label: None, span_first: usize::MAX, span_last: 0, row_start: row, else_row: None, row_end: row }`. `else <cond>` sets the top frame's `else_label`/`else_row = row`. `end` pops the top frame and sets `row_end = row.saturating_sub(1).max(row_start)`.
- Every message/note updates the innermost open frame's `span_first = min(span_first, min(cols))`, `span_last = max(span_last, max(cols))` for the columns it touches, so the frame ends up spanning its content.
- Line parsing:
  - `participant ID as Label` / `participant ID` / `actor …` → participant (dedup).
  - A message: find the arrow token (`-->>` before `->>`; also accept `->>`/`-->>`), split `left ARROW right : text`. `from=get(left)`, `to=get(right)`, `kind` by arrow (`--` prefix → Dashed), `self_msg = from==to`, `row` = current. Use `message_colon`-style split for the `: text` (a plain `:`), reusing a local helper or `str::split_once(':')` on the part after the arrow.
  - `alt`/`else`/`end` as above (stray `else`/`end` with empty stack: ignore).
  - `Note over A,B: text` / `Note over A: text` → Note spanning those participants at the current row (advance row).
  - `%%` comments stripped; `title …`, `autonumber`, `loop`/`opt`/`par` + their `end` — for THIS slice, treat unknown block openers' `end` as frame `end` only if a frame is open (documented: nested non-alt blocks aren't drawn). Skip `title`/`autonumber`.
- `is_sequence`: first non-empty non-`%%` line lowercased == `"sequencediagram"`.
- Set `SequenceDiagram.rows = row` (total row count).

- [ ] **Step 4: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_seq`

- [ ] **Step 5: fmt/clippy; commit.** (`#[allow(dead_code)]` on not-yet-consumed layout fields is fine.)

```bash
git commit -am "docxcore: sequence-diagram parser + model (mermaid_seq)"
```

---

### Task 2: Layout + SequenceGeometry + JSON

Lay the model out in EMU and produce the serializable geometry both renderers consume.

**Files:** Modify `docxcore/src/mermaid_seq.rs`

**Interfaces:**
- Consumes: the Task-1 model.
- Produces:
  - `fn layout(d: &mut SequenceDiagram)` — fills `Participant.x/w` and exposes row/height metrics (module consts + helpers).
  - `pub struct SequenceGeometry { pub canvas_w: i64, pub canvas_h: i64, participants, lifelines, messages, frames, notes … }` with `pub fn to_json(&self) -> String` emitting `{"kind":"sequence","canvasW":…,"canvasH":…,"participants":[{x,y,w,h,label}],"lifelines":[{x,y1,y2}],"messages":[{x1,y1,x2,y2,text,dashed,self}],"frames":[{x,y,w,h,label,elseLabel,elseY}],"notes":[{x,y,w,h,text}]}`.
  - `pub fn geometry(src: &str) -> SequenceGeometry` (parse + layout + build).

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn layout_columns_and_rows() {
    let g = geometry("sequenceDiagram\nparticipant A\nparticipant B\nparticipant C\nA->>B: m1\nB->>C: m2");
    assert_eq!(g.participants.len(), 3);
    // Columns strictly increasing, non-overlapping.
    assert!(g.participants[0].x + g.participants[0].w <= g.participants[1].x);
    assert!(g.participants[1].x + g.participants[1].w <= g.participants[2].x);
    // A lifeline per participant, spanning below the header.
    assert_eq!(g.lifelines.len(), 3);
    assert!(g.lifelines[0].y2 > g.lifelines[0].y1);
    // Two messages, monotonically increasing y (row order).
    assert_eq!(g.messages.len(), 2);
    assert!(g.messages[1].y1 > g.messages[0].y1);
    assert!(g.canvas_w > 0 && g.canvas_h > 0);
}

#[test]
fn json_tagged_sequence() {
    let j = geometry("sequenceDiagram\nA->>B: hi").to_json();
    assert!(j.contains("\"kind\":\"sequence\""));
    assert!(j.contains("\"lifelines\":[") && j.contains("\"messages\":["));
}

#[test]
fn frame_box_spans_participants_and_rows() {
    let g = geometry("sequenceDiagram\nparticipant A\nparticipant B\nalt c\n A->>B: y\nelse d\n B->>A: z\nend");
    assert_eq!(g.frames.len(), 1);
    let f = &g.frames[0];
    // Spans from A's column to B's column, has an else divider inside its height.
    assert!(f.w > 0 && f.h > 0);
    assert!(f.else_y.unwrap() > f.y && f.else_y.unwrap() < f.y + f.h);
}
```
(Match the geometry field names you choose; keep them consistent with `to_json`.)

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_seq`

- [ ] **Step 3: Implement layout + geometry.** EMU metrics (reuse/echo `mermaid`'s `EMU_PER_INCH`): header box height ~0.5", column width from `max(label_width, message_label_widths at that column)` clamped (min ~1.3", max ~2.6"), column gap ~0.4"; row step ~0.5", self-message row taller (~0.75"); top margin = header height + gap. Participant `x` = running left; lifeline `x` = `participant.x + w/2`, `y1` = header bottom, `y2` = canvas bottom − margin. Message `y` = top margin + `row*step + step/2`; `x1`/`x2` = from/to lifeline x (self-message: a loop to the right of its own lifeline). Frame rect: x from `participants[span_first].x` − pad to `participants[span_last].x + w` + pad; y from `row_start` row-top − pad to `row_end` row-bottom + pad; `else_y` = the `else_row` row-top. Note rect: spans `participants[span_first..=span_last]` at its row. `canvas_w` = last participant right + margin (and max over frame/note right); `canvas_h` = top margin + rows*step + bottom margin. Build `SequenceGeometry` and `to_json` (hand-rolled, escape strings — you may make `mermaid::xml_escape_text` etc. `pub(crate)` in a later task; for JSON string escaping mirror `mermaid`'s `json_str`/or add a local one).

- [ ] **Step 4: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_seq`

- [ ] **Step 5: fmt/clippy; commit.**

```bash
git commit -am "docxcore/mermaid_seq: layout + SequenceGeometry + kind-tagged JSON"
```

---

### Task 3: DrawingML emit

Emit a `<w:drawing>` for the sequence diagram (Word shapes), reusing the flowchart emitter's group wrapper + escaping via shared `pub(crate)` helpers.

**Files:** Modify `docxcore/src/mermaid_seq.rs`, `docxcore/src/mermaid.rs` (make shared helpers `pub(crate)`; extract the drawing-group wrapper)

**Interfaces:**
- Consumes: Task-2 layout/geometry; shared `mermaid` helpers.
- Produces: `pub fn to_drawing(src: &str) -> (String, Vec<String>)` (xml + caption lines = participant labels); shared `pub(crate) fn mermaid::wrap_drawing_group(shapes: &str, w: i64, h: i64, src: &str) -> String` (the `mc:AlternateContent`/`wpg:wgp` wrapper + `descr` embedding), called by BOTH `mermaid::emit_drawing` and `mermaid_seq::to_drawing`.

- [ ] **Step 1: Extract the shared wrapper (no behavior change).** In `mermaid.rs`, factor the `mc:AlternateContent … wpg:wgp … {shapes} … descr="mermaid:…"` wrapper out of `emit_drawing` into `pub(crate) fn wrap_drawing_group(shapes: &str, w: i64, h: i64, src: &str) -> String`; have `emit_drawing` call it. Make `xml_escape_text`, `xml_escape_attr`, `escape_source`, and `MARKER` `pub(crate)`. Run the existing flowchart tests — `emits_drawingml_with_shapes_and_connector`, `source_embeds_and_round_trips`, etc. must stay green (byte-identical wrapper output).

- [ ] **Step 2: Write failing sequence-emit tests.**

```rust
#[test]
fn emits_participant_boxes_and_lifelines() {
    let (xml, text) = to_drawing("sequenceDiagram\nparticipant A as Alice\nparticipant B as Bob\nA->>B: hi");
    assert!(xml.contains("<w:drawing>"));
    assert!(xml.contains("Alice") && xml.contains("Bob"));
    // A dashed lifeline (prstDash) and a message arrow (tailEnd) present.
    assert!(xml.contains("prstDash"), "lifeline dash missing: {xml}");
    assert!(xml.contains("tailEnd"), "arrow head missing");
    assert_eq!(text, vec!["Alice".to_string(), "Bob".to_string()]);
}

#[test]
fn emits_alt_frame_and_note() {
    let (xml, _) = to_drawing("sequenceDiagram\nA->>B: x\nalt c\n A->>B: y\nend\nNote over A,B: n");
    assert!(xml.contains("roundRect")); // frame (and/or note) box
    assert!(xml.contains(">c<") || xml.contains("preserve\">c")); // alt label
    assert!(xml.contains(">n<") || xml.contains("preserve\">n")); // note text
}

#[test]
fn sequence_source_round_trips() {
    let src = "sequenceDiagram\nA->>B: hi";
    let (xml, _) = to_drawing(src);
    assert!(xml.contains("descr=\"mermaid:"));
    assert_eq!(crate::mermaid::source_of(&xml).as_deref(), Some(src));
}
```

- [ ] **Step 3: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_seq`

- [ ] **Step 4: Implement `to_drawing`.** Build the `{shapes}` string, then `mermaid::wrap_drawing_group(&shapes, canvas_w, canvas_h, src)`. Shape order (z-order): frame rects (behind) → note rects → participant boxes → lifelines → messages+labels. Emit:
- Participant box: `wps:wsp` `rect` with label run (reuse the flowchart node style, light fill).
- Lifeline: a thin vertical connector/line `wps:wsp` with `<a:prstGeom prst="line">` (or a straight `cxnSp`) + `<a:ln><a:prstDash val="dash"/></a:ln>` from `(x,y1)` to `(x,y2)`.
- Message: a horizontal arrow — a `custGeom`/`line` from `(x1,y)` to `(x2,y)` with `<a:tailEnd type="triangle"/>` and `prstDash` when `MsgKind::Dashed`; self-message = a small 3-point loop to the right of the lifeline. Label text run centered above the line (a small text box).
- Frame: `roundRect`, light fill, thin stroke, title tab (a small run top-left); `else` → a divider line at `else_y` + the `[else]` label.
- Note: `rect` with a distinct fill (e.g. `FFF6D5`) + text run.
- Caption `text_lines` = participant labels (for the terminal/PDF fallback box).

- [ ] **Step 5: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (whole crate — confirms the wrapper refactor didn't regress flowcharts).

- [ ] **Step 6: fmt/clippy; commit.**

```bash
git commit -am "docxcore/mermaid_seq: DrawingML emit; share drawing-group wrapper"
```

---

### Task 4: Dispatch wiring (Word + view_json flow end-to-end)

Route a `sequenceDiagram` source to `mermaid_seq` from the shared entry points so it reaches both Word and the webview geometry.

**Files:** Modify `docxcore/src/mermaid.rs`, `docxcore/src/render.rs`; Test in `docxwasm/src/bridge.rs`

**Interfaces:**
- Consumes: `mermaid_seq::is_sequence`/`to_drawing`/`geometry`.
- Produces: `mermaid::to_drawing` dispatches; `pub fn mermaid::geometry_box(src: &str) -> (i64, i64, String)` (canvas w, h, geometry JSON) dispatching on kind; `render.rs` uses `geometry_box`.

- [ ] **Step 1: Write the failing bridge/dispatch tests.**

```rust
// docxcore/src/mermaid.rs tests
#[test]
fn to_drawing_dispatches_sequence() {
    let (xml, _) = to_drawing("sequenceDiagram\nA->>B: hi");
    assert!(xml.contains("prstDash")); // lifeline → sequence path
    // A flowchart is unaffected.
    let (fx, _) = to_drawing("flowchart TD\nA-->B");
    assert!(fx.contains("<a:custGeom>")); // flowchart connector, unchanged
}

#[test]
fn geometry_box_tags_kind() {
    let (w, h, json) = geometry_box("sequenceDiagram\nA->>B: hi");
    assert!(w > 0 && h > 0);
    assert!(json.contains("\"kind\":\"sequence\""));
    let (_, _, fj) = geometry_box("flowchart TD\nA-->B");
    assert!(fj.contains("\"nodes\":[")); // flowchart geometry
}
```
```rust
// docxwasm/src/bridge.rs tests
#[test]
fn view_json_sequence_kind() {
    let doc = docxcore::markdown::from_markdown("```mermaid\nsequenceDiagram\nA->>B: hi\n```\n");
    let mut s = /* same Session constructor the neighboring tests use */;
    let v = s.view_json(None);
    assert!(v.contains("\"mermaid\":["));
    assert!(v.contains("\"kind\":\"sequence\""));
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore to_drawing_dispatches geometry_box && cargo test -p docxwasm view_json_sequence`

- [ ] **Step 3: Implement dispatch.**
- `lib.rs`: ensure `pub mod mermaid_seq;` (added in Task 1).
- `mermaid::to_drawing`: at the top, `if crate::mermaid_seq::is_sequence(src) { return crate::mermaid_seq::to_drawing(src); }`.
- Add `pub fn mermaid::geometry_box(src: &str) -> (i64, i64, String)`: `if is_sequence(src) { let g = mermaid_seq::geometry(src); (g.canvas_w, g.canvas_h, g.to_json()) } else { let g = geometry(src); (g.canvas_w, g.canvas_h, g.to_json()) }`.
- `render.rs:1588-1599`: replace the three `geo.*` uses with one call: `let (cw, ch, gjson) = crate::mermaid::geometry_box(&src); let (cols, rows) = mermaid_box_cells(cw, ch, width); … geometry_json: gjson`.
- `mermaid::labels` (terminal fallback) may optionally dispatch to participant labels; not required for this task.

- [ ] **Step 4: Run — expect PASS + no regressions.**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p docxcore
cargo test -p docxwasm
```

- [ ] **Step 5: Rebuild wasm + confirm gates.** `cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:mcp-parity` (56/56).

- [ ] **Step 6: fmt/clippy; commit.**

```bash
git commit -am "docxcore: route sequenceDiagram to mermaid_seq (to_drawing + geometry_box)"
```

---

### Task 5: Webview `buildSequenceSvg` + real-doc regression

Render the sequence geometry as an inline SVG in the webview, matching Word.

**Files:** Modify `offxy-vscode/media/webview.js`, `offxy-vscode/media/mermaid-svg.test.mjs`

**Interfaces:**
- Consumes: `view_json` `mermaid[].geo` with `kind:"sequence"`.
- Produces: `buildSequenceSvg(geo)`; `buildMermaidSvg` branches on `geo.kind`.

- [ ] **Step 1: Write the failing JS test (`mermaid-svg.test.mjs`).** Add a sequence geometry fixture (2 participants, 1 lifeline each, 1 message, 1 frame with elseY, 1 note) and assert `buildSequenceSvg(geo)` (or `buildMermaidSvg` with `kind:"sequence"`) yields: 2 participant `<rect>`, 2 lifeline `<line>` with `stroke-dasharray`, ≥1 message `<polyline>`/`<line>` with an arrow marker, 1 frame `<rect>` + a divider line, 1 note `<rect>`, and the labels/texts as escaped `<text>`.

```js
const seqGeo = { kind:'sequence', canvasW:3000000, canvasH:2000000,
  participants:[{x:0,y:0,w:900000,h:400000,label:'A'},{x:1500000,y:0,w:900000,h:400000,label:'B'}],
  lifelines:[{x:450000,y1:400000,y2:1900000},{x:1950000,y1:400000,y2:1900000}],
  messages:[{x1:450000,y1:700000,x2:1950000,y2:700000,text:'m1',dashed:false,self:false}],
  frames:[{x:200000,y:550000,w:2000000,h:900000,label:'c',elseLabel:'d',elseY:1000000}],
  notes:[{x:300000,y:1500000,w:1800000,h:300000,text:'n & <ote>'}] };
```

- [ ] **Step 2: Run — expect FAIL.** `cd offxy-vscode && node media/mermaid-svg.test.mjs`

- [ ] **Step 3: Implement.** In `webview.js`, `buildMermaidSvg(geo)` → `if (geo.kind === 'sequence') return buildSequenceSvg(geo);` (keep the existing flowchart body for the default/`kind:"flowchart"`|undefined case). `buildSequenceSvg(geo)`: `<svg viewBox="0 0 canvasW canvasH">` with, in z-order: frame `<rect>` (light fill) + title `<text>` + `[elseLabel]` divider `<line>`/text at `elseY`; note `<rect>` (distinct fill) + text; participant `<rect>` + centered label; lifelines `<line stroke-dasharray="6 6">`; messages `<polyline>`/`<line>` + arrow `<marker>` (dashed via `stroke-dasharray` when `m.dashed`; self-message a small loop path); message label `<text>` above the line. Escape all text (reuse the existing `escMermaidText`). Reuse the existing overlay wiring (a sequence box is just another `mermaid[]` entry — no overlay-path change; it already calls `buildMermaidSvg`).

- [ ] **Step 4: Run JS test — expect PASS.** `cd offxy-vscode && node media/mermaid-svg.test.mjs`

- [ ] **Step 5: Real-doc regression.** Rebuild the CLI and convert the committed sample:
```bash
cd C:/Users/boris_kudriashov/Source/docxy && export PATH="$HOME/.cargo/bin:$PATH" && cargo build --release -p docxy
target/release/docxy.exe offxy-vscode/samples/mermaid/12-aliaksei-provisioning-seq.md --docx /tmp/seq.docx
unzip -p /tmp/seq.docx word/document.xml | grep -c '<wps:wsp>'   # many shapes (participants+lifelines+messages+frame)
```
Confirm via `unzip -p /tmp/seq.docx word/document.xml`: 6 participant labels present (U/DL/EB/DI/DLe/PV aliases), `prstDash` lifelines present, the `alt` label text present, and the source round-trips (`descr="mermaid:`). (The `.docx` is throwaway — gitignored/tmp, do not commit.)

- [ ] **Step 6: Full gate.**
```bash
cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH"
npm run typecheck && npm run build && npm run test:mermaid-svg && npm run test:md-roundtrip && npm run test:grid-layout && npm run test:mcp-parity
```
All green; mcp 56/56.

- [ ] **Step 7: Commit.**

```bash
git add offxy-vscode/media/webview.js offxy-vscode/media/mermaid-svg.test.mjs
git commit -m "offxy webview: render sequence diagrams as inline SVG (matches Word)"
```

---

## Notes for the executor

- Tasks are sequential: 1 (parse) → 2 (layout/geometry) → 3 (emit + shared wrapper) → 4 (dispatch, makes it flow) → 5 (webview). The diagram only renders end-to-end after Task 4 (Word) and Task 5 (webview).
- The flowchart engine must stay byte-identical: Task 3's wrapper extraction is the only `mermaid.rs` output-adjacent change — the existing flowchart tests are the guard.
- Do NOT add activations/loop/opt/par/autonumber/actors or non-`over` notes — all out of scope (future slice).
- The sequence path reuses the `mermaid[]` view_json array and `MermaidBox` cell-anchoring unchanged; only the geometry JSON shape and the webview SVG branch are new.
