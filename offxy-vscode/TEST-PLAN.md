# offxy VS Code — Manual Test Plan

Covers the live-VS-Code behavior that automated tests can't reach: the grid
webview UI, and the agent MCP surface driving open document/workbook tabs.
Scope = PR #28 (agent access + Waves 1–3 + grid webview overhaul).

Two independent parts:
- **Part B (Grid UI)** needs only the installed extension — no agent.
- **Part C+ (Agent surface)** needs an MCP client (Claude Code or Copilot agent
  mode) connected to the bundled `offxy` MCP server.

Mark each item ✅ / ❌ and note anything surprising.

---

## A. Setup / preconditions

- [ ] **A1** — Build & install the vsix: from `offxy-vscode/`, `npm run build`,
  `npx --yes @vscode/vsce@latest package --no-dependencies`, then
  `code --install-extension offxy-0.3.0.vsix --force`. Reload VS Code.
- [ ] **A2** — `node` is on your PATH (`node --version` in a plain terminal).
  The bundled MCP server is launched as `node …`; without it, Copilot/Claude
  Code won't discover the tools.
- [ ] **A3** — Sample files ready in a scratch folder: a small `.docx`, a
  small `.xlsx` with a few rows of data (some numbers), and an empty
  0-byte file renamed `blank.xlsx` (for the empty-state test).
- [ ] **A4** — An MCP client for Part C+: either Claude Code with the `offxy`
  server registered, or VS Code Copilot **agent mode** (the extension
  registers the server automatically via the VS Code MCP API).

---

## B. Grid webview UI (extension only — no agent)

Open the sample `.xlsx` in a VS Code tab for all of these.

### B1. Layout & rendering
- [ ] **B1.1 Gutter alignment** — The value in **A1** sits directly *under* the
  "A" column header and to the *right* of the "1" row header — not hidden
  behind the sticky header bars. Row 1 and column A are fully visible.
- [ ] **B1.2 Full-viewport gridlines** — Gridlines cover the *whole* visible
  area, including empty cells — not only where data sits. Scroll around: the
  grid stays filled.
- [ ] **B1.3 Value outside the used range** — Type a value into a cell well
  below/left of your data (e.g. `A20`). It lands in a proper gridded cell
  (not a floating bare box); the scroll area extends to include it.
- [ ] **B1.4 Sticky headers on scroll** — Scroll down/right: the column/row
  headers stay pinned and their labels track the visible cells; cell content
  fills in after scrolling settles.
- [ ] **B1.5 Header/click round-trip** — Click a cell; the highlighted cell and
  the formula-bar reference (`B4` etc.) match the cell you clicked, at every
  scroll position.

### B2. Selection & active cell
- [ ] **B2.1 Active-cell highlight** — Select a multi-cell range (click-drag or
  shift-click). The range is tinted **except the active (anchor) cell**, which
  shows the normal cell background and stands out. (This was the last fix.)
- [ ] **B2.2 Range border** — The selection has a single outer border marking
  its extent; the active cell keeps its own box.
- [ ] **B2.3 Keyboard nav** — Arrow keys move the active cell; Shift+Arrow
  extends the selection; PageUp/PageDown jump; Ctrl+Home → A1. Scrolling
  follows so the active cell stays visible (and clears the sticky headers).
- [ ] **B2.4 Drag select** — Click-drag across a range: it selects smoothly
  with no visible jank/flicker (the gridline backdrop must not rebuild per
  mouse-move).

### B3. Editing
- [ ] **B3.1 Type-through** — Select a cell, start typing: an editor opens with
  the typed text. Enter commits & moves down; Tab commits & moves right;
  Escape cancels.
- [ ] **B3.2 F2 / double-click** — F2 or double-click opens the editor with the
  cell's current content for editing.
- [ ] **B3.3 Formula bar** — Type into the formula bar; Enter commits, Escape
  reverts. It reflects the active cell's source.
- [ ] **B3.4 Formula + recalc** — Enter `=SUM(A1:A3)` (or similar) into a cell;
  it evaluates and shows the result; changing a source cell updates it.
- [ ] **B3.5 Clipboard** — Ctrl+C / Ctrl+X / Ctrl+V (or the toolbar buttons)
  copy/cut/paste a range, including multi-cell TSV; paste from Excel/another
  app works.

### B4. Toolbar
- [ ] **B4.1 Renders** — A toolbar sits above the formula bar with groups:
  Cut/Copy/Paste · AutoSum · **B** *I* · font color · fill color · align
  L/C/R · $ % `,` · increase/decrease decimals.
- [ ] **B4.2 Bold/Italic toggle** — Select a range, click **B** → all cells
  bold; click again → un-bold. Same for *I*.
- [ ] **B4.3 Pressed state** — Move the active cell onto a bold cell: the **B**
  button shows a pressed/active state; onto a plain cell: not pressed. Same
  for Italic and the three align buttons.
- [ ] **B4.4 Colors** — Font-color / fill-color buttons open a color picker;
  choosing a color applies it to the selection.
- [ ] **B4.5 Align** — Left/center/right change the selected cells' alignment.
- [ ] **B4.6 Number formats** — `$` → currency, `%` → percent, `,` → comma;
  increase/decrease-decimals add/remove a decimal place (try on a plain
  number, a currency, and a percent — the affix is preserved).
- [ ] **B4.7 AutoSum** — Put numbers in a column, select the empty cell below,
  click Σ → it writes `=SUM(<the column above>)` and evaluates.
- [ ] **B4.8 AutoSum no-op** — Click Σ on a cell with nothing to sum: nothing
  is written, and — importantly — a following **Ctrl+Z does not revert an
  unrelated earlier edit** (no phantom undo entry).
- [ ] **B4.9 Toolbar edit undo** — Any toolbar format (bold, color, number
  format…) is reverted by a single Ctrl+Z, like a normal edit.
- [ ] **B4.10 Selection kept** — Clicking a toolbar button does not move or
  clear your selection.

### B5. Structure & lifecycle
- [ ] **B5.1 Sheet tabs** — Switch sheets via the bottom tabs; Ctrl+T (or the
  add affordance) adds a sheet; rename a sheet.
- [ ] **B5.2 Row/col insert/delete** — Right-click a row/column header →
  insert/delete; formulas referencing shifted cells update correctly.
- [ ] **B5.3 Empty-file state** — Open the 0-byte `blank.xlsx`: instead of a
  grid, a "This file is empty… Create new workbook" prompt appears; clicking
  it turns the tab into a real editable workbook.
- [ ] **B5.4 Save / dirty dot** — Make an edit → the tab shows the dirty dot;
  Ctrl+S saves; reopening the file shows the saved content.
- [ ] **B5.5 Ctrl+Z/Y/S passthrough** — With a cell selected (and separately,
  with the in-cell editor focused), Ctrl+Z / Ctrl+Y / Ctrl+S reach VS Code's
  undo/redo/save rather than being swallowed by the grid.

*(Repeat the docx tab's basic open/edit/save with the sample `.docx` — it uses
the docx webview, not the grid, but confirm it opens, renders, edits, and
saves.)*

---

## C. Agent surface — discovery & basics (needs an MCP client)

Open the sample `.docx` **and** `.xlsx` in tabs first.

- [ ] **C1 Tool discovery** — In Copilot agent mode (or `claude mcp` list), the
  `offxy` server exposes **56 tools** (`docxy_*` + `xlsxy_*`). Ask the agent to
  "list your offxy tools" and confirm the count/names.
- [ ] **C2 Instance discovery** — Ask the agent to run `docxy_list` /
  `xlsxy_list`. Each open tab appears as a `docxy-vscode-…` / `xlsxy-vscode-…`
  instance.
- [ ] **C3 No instance** — Close all xlsx tabs, ask for `xlsxy_list` → empty;
  a verb needing a target returns a clear "no running xlsxy" error.
- [ ] **C4 Read matches the tab** — `docxy_read` / `xlsxy_read` returns exactly
  what's shown in the tab, **including unsaved edits** (make an edit first,
  don't save, then read — the edit is present).

---

## D. Agent → docx tab

- [ ] **D1 Live edit + undo lockstep** — Ask the agent to `docxy_replace_range`
  a paragraph. The tab updates live, the dirty dot lights, and **one Ctrl+Z**
  reverts exactly that edit (not more, not less).
- [ ] **D2 Empty-paragraph replace** — Put the caret on an empty paragraph;
  have the agent replace that range. One Ctrl+Z fully reverts it and the undo
  stack stays in step (this was a Critical-bug scenario — verify no double
  undo / no corruption).
- [ ] **D3 Markdown write renders in Word** — On a **freshly created** doc
  (`docxy_new`), have the agent `docxy_insert` with `markdown:true` a heading +
  a bulleted list + a table. In the tab it renders formatted; **save and open
  in real Word** — the Heading shows the actual Heading style, the list has
  markers, the table is a table.
- [ ] **D4 doc.format / set-style** — Agent bolds a run range and sets a
  paragraph to Heading1; renders in the tab, one Ctrl+Z each reverts.
- [ ] **D5 export** — `docxy_export` (markdown) returns the document as
  markdown; `docxy_export_pdf` writes an openable PDF.
- [ ] **D6 doc.open makes a NEW tab** — Agent `docxy_*` open of another file
  opens it as a **new tab** (documented deviation from terminal in-place swap).
- [ ] **D7 Large CJK/emoji read** — Put a lot of non-ASCII text (Cyrillic/CJK/
  emoji) in a doc; `docxy_read` through the agent round-trips it intact (no
  `�` replacement characters — this exercises the split-UTF-8 fix).

---

## E. Agent → xlsx tab

- [ ] **E1 range.set live + atomic** — Agent `xlsxy_range_set` a 2×2 block; it
  appears live, one Ctrl+Z restores all cells. A batch containing one invalid
  formula is rejected whole (nothing applied).
- [ ] **E2 cell.format + read-back** — Agent `xlsxy_format` (bold/fill/number
  format) a range; the tab shows it; `xlsxy_get` reports the format; **only**
  `xlsxy_get` carries format (a `xlsxy_read` of the same cells doesn't). One
  Ctrl+Z reverts.
- [ ] **E3 Comments round-trip** — Agent `xlsxy_comment_add` on a cell → the
  comment appears; **one Ctrl+Z restores the prior thread exactly** (if the
  cell already had a conversation, the agent's add-then-undo doesn't wipe it).
- [ ] **E4 Ad-hoc pivot / analysis** — `xlsxy_pivot` (read-only) returns a
  computed table without changing the workbook; `xlsxy_eval` previews a
  formula without mutating.
- [ ] **E5 CSV** — `xlsxy_export_csv` returns the sheet as CSV;
  `xlsxy_import_csv` adds a **new** sheet from CSV text (never overwrites).

### E6. Pivot create + undo/redo (the both-or-neither chain)
- [ ] **E6.1 Create** — Agent `xlsxy_pivot_create` over a data range → a **new
  sheet** appears with the pivot; it shows in `xlsxy_pivots`.
- [ ] **E6.2 Refresh** — Edit a source cell, agent `xlsxy_recalc` → the pivot's
  output sheet updates.
- [ ] **E6.3 Undo removes both** — Ctrl+Z → the pivot's sheet **and** its
  registration both go away together (no dangling pivot).
- [ ] **E6.4 Redo restores both** — Ctrl+Y → sheet and pivot both return, and
  the restored pivot still refreshes on a source edit + recalc.

### E7. Structural undo edge cases (reviews flagged these)
- [ ] **E7.1 Sheet remove → restore** — Agent removes a data-bearing sheet
  (with comments and any pivot); Ctrl+Z restores its content, comments, and
  the pivot — **appended at the end** of the tab order (documented).
- [ ] **E7.2 Double remove, double undo** — Agent removes two sheets in a row;
  press Ctrl+Z twice. The second undo **surfaces a warning** (single-slot
  restore) rather than silently doing the wrong thing.
- [ ] **E7.3 Remove → import → undo → undo** — Agent removes a sheet, then
  imports a CSV (new sheet); Ctrl+Z (undo import) then Ctrl+Z again. The
  second undo shows a **mismatch warning** and does **not** silently restore
  the wrong sheet.

---

## F. Cross-cutting

- [ ] **F1 Two windows, same basename** — Open two VS Code windows, each with a
  different `report.xlsx` from different folders. `xlsxy_list` shows **two
  distinct instances** (pid-suffixed); an agent edit targets the right one.
- [ ] **F2 doc.reload dirty flag** — After an agent reload, the tab re-reads
  from disk but VS Code's dirty flag stays set (documented quirk).
- [ ] **F3 Terminal parity (optional)** — If you also run terminal
  `docxy --mcp` / `xlsxy --mcp`, the same tool call gives the same reply shape
  from a tab as from the terminal (a tab is indistinguishable).
- [ ] **F4 node-off-PATH** — Temporarily remove `node` from PATH and reload:
  Copilot no longer discovers the tools (confirms the documented requirement),
  then restore PATH.

---

## Notes / known limitations (expected, not bugs)

- `doc.open` / `xlsxy` open verbs make a **new tab** rather than swapping the
  current file in place.
- After a markdown write is undone, the ensured styles/numbering definitions
  remain in the package (not checkpointed) — harmless.
- A **filled** cell inside a selection doesn't show the selection tint over its
  fill (the selection border still marks it) — a follow-up item.
- Not in this build (need engine work): grid Sort, borders, merge, underline.

## Result summary

| Section | Pass | Fail | Notes |
|---|---|---|---|
| A Setup | | | |
| B Grid UI | | | |
| C Agent basics | | | |
| D Docx agent | | | |
| E Xlsx agent | | | |
| F Cross-cutting | | | |
