# Offxy JetBrains Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Revision note (2026-07-21, after Tasks 1–2):** rendering moved from a
> custom-painted panel to a real IntelliJ editor over an **editable Document
> the engine follows** (see the revised spec). Tasks 3–6 below reflect the
> new architecture; Tasks 1–2 predate the revision and stand as committed.

**Goal:** A native JetBrains plugin that edits `.docx` in an IDE tab — the IntelliJ editor as the surface, the shared `docxwasm.wasm` engine (on Chicory) as the model — with native typing latency, platform-owned undo/find, and per-tab agent ctl access.

**Architecture:** The engine's grid render is the editor's Document text; span styles are RangeHighlighters; decorations are guarded blocks; user edits replay into the engine through a DocumentListener (offset→segment mapping + `click`/`insert`/`delete`/`paste` commands); the engine's authoritative render reconciles the Document by minimal line diff (empty in the no-rewrap case). Engine sync is async off the keystroke path, so felt latency is native regardless of document size (retires the Task 2 perf-gate concern). Formatting commands and agent ctl edits reconcile the same way and carry snapshot-based `UndoableAction`s.

**Tech Stack:** Kotlin/JVM 17, IntelliJ Platform Gradle Plugin 2.18.1 (IC 2024.2, since-build 242), Chicory 1.4.0, Rust (one small docxwasm addition).

**Spec:** `docs/superpowers/specs/2026-07-21-offxy-jetbrains-design.md` (revised)
**References:** `docxwasm/src/bridge.rs` (`view_json`, `LineMap`), `offxy-vscode/src/extension.ts` (host duties being mirrored), `docs/agent-control.md` + `docs/superpowers/plans/2026-07-17-offxy-agent-access.md` (ctl wire + `docx_ctl`).

## Global Constraints

- **No behavior change** to the terminal apps or the VS Code extension. The only Rust change is Task 3's `segs` field (additive; all existing tests stay green; webview ignores unknown JSON fields — verify).
- Workspace version stays put; no bumps — release is Boris's call.
- Runtime dependency rule: **Chicory only**.
- Wire compatibility for Task 7: EXACTLY ctlcore's protocol; discovery files `{"instance","port","token","pid"}` in `%APPDATA%\docxy\ctl` / `$XDG_CONFIG_HOME/docxy/ctl`.
- Gates: `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -p docxwasm`; `./gradlew build buildPlugin` (runs all Kotlin tests).
- **The engine is always authoritative.** Any mapping ambiguity or engine error resolves as a full re-render reconcile, never by trusting the Document.
- **JDK/Gradle:** installed (scoop: temurin17-jdk, gradle). Gradle runs via `./gradlew` from `offxy-jetbrains/` with `JAVA_HOME=/c/Users/boris/scoop/apps/temurin17-jdk/current`.
- **Windows agent shell quirks:** every cargo command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail` (`cmd > log 2>&1; echo exit=$?` instead).

---

### Task 1: Scaffold the Gradle plugin project

> **Status: DONE** (commit `3981624`). JDK 17 + Gradle via scoop; wasm32
> target had to be `rustup target add`ed; wasm ships via
> `processResources { from(buildWasm) }` (a Copy into src/main/resources trips
> Gradle's implicit-dependency validation against `patchPluginXml`).

---

### Task 2: `DocxEngine` + `ChicoryEngine`, tests, and the performance gate

> **Status: DONE** (commit `e4a2c6a`). All engine tests green. Benchmark:
> 0.35 ms small / 6.7 ms typical 20 KB / 62 ms on the 220 KB stress doc —
> linear in doc size (full render per command). Originally a gate miss;
> **retired by the editable-Document revision**: engine sync is async
> catch-up, felt typing latency is native. The benchmark stays as the
> catch-up-latency measurement.

---

### Task 3: `segs` in `view_json` (Rust)

> **Status: DONE** (commit `d3aae9e`). 16 docxwasm tests green.

**Files:**
- Modify: `docxwasm/src/bridge.rs`

**Interfaces:**
- Consumes: `Session::view_json` and the `LineMap`/seg structures it already
  builds (`maps` — the same data click resolution uses).
- Produces: each rendered line's entry in the view JSON gains its editable
  column ranges. Emit as a parallel top-level array `"segs":[[[c0,c1],…],…]`
  (one list per line, `c0` inclusive/`c1` exclusive, empty list for
  decoration-only lines). Tasks 4–5 consume it for guarded blocks and offset
  mapping. The webview ignores it (verify no webview breakage by inspection —
  it indexes known fields only).

- [ ] **Step 1: Failing tests** in bridge.rs's style: plain paragraph (one seg
  spanning the text), wrapped paragraph (one seg per visual line), bulleted
  list (seg starts after the marker), table row (segs exclude border
  columns), image-only region (empty seg lists).
- [ ] **Step 2: Implement** — serialize from the existing maps in
  `view_json`; no new computation.
- [ ] **Step 3:** `cargo test -p docxwasm` green; fmt/clippy; wasm32 release
  build; extend the Kotlin `ViewModel` test fixture expectations (Task 4 will
  parse it).
- [ ] **Step 4: Commit** — `"docxwasm: expose editable segment ranges in the view JSON"`

---

### Task 4: `ViewModel` + `EditorView` — render into a real editor

> **Status: DONE** (commit `d3ff38f`), except the `runIde` smoke (pending a
> human eye). Guard enforcement proven in the platform test via
> `startGuardedBlockChecking`. ANSI colors are fixed JBColor pairs for now
> (scheme-derived mapping noted as follow-up).

**Files:**
- Create: `editor/ViewModel.kt`, `editor/EditorView.kt`, `editor/DocxFileEditor.kt`, `editor/DocxEditorProvider.kt`
- Modify: `plugin.xml` (fileEditorProvider registration)

**Interfaces:**
- Consumes: Task 2's `DocxEngine`; Task 3's `segs`; IntelliJ `EditorFactory`
  (real `Editor` over a standalone `Document`), `MarkupModel`/
  `RangeHighlighter`/`TextAttributes`, `Document.createGuardedBlock`,
  `EditorColorsManager` + console ANSI color keys, `EditorCustomElementRenderer`
  or `CustomHighlighterRenderer` for images, `ImageIO`.
- Produces:
  ```kotlin
  class ViewModel(json: String) {            // lines, spans, caret, dirty, images, segs
    val text: String                          // lines joined with \n
    fun spanRanges(): List<StyledRange>       // absolute offsets + style flags/color
    fun guardRanges(): List<IntRange>         // complement of segs, per line, absolute
    fun offsetToGrid(offset: Int): Pair<Int, Int>
    fun gridToOffset(line: Int, col: Int): Int
  }
  class EditorView(project: Project, engine: DocxEngine) {
    val editor: Editor                        // editable Document, we own lifecycle
    fun applyRender(view: ViewModel)          // full apply: text, highlighters, guards, images
    fun reconcile(view: ViewModel)            // minimal line-diff patch (self-write flagged)
    var suppressListener: Boolean             // EditPipeline's self-write guard (Task 5)
  }
  ```
  `DocxFileEditor` wires file bytes → engine → `applyRender`; provider accepts
  `*.docx` with `HIDE_DEFAULT_EDITOR`; dispose releases the editor and engine.
  Editable Document from day one (Task 5 adds the listener; until then edits
  are visually unreconciled, which is fine mid-plan).

- [ ] **Step 1:** `ViewModel` + unit tests against captured render JSON from a
  Task 2 fixture (spans, offsets round-trip, guard complement of segs).
- [ ] **Step 2:** `EditorView.applyRender` (text + highlighters + guards +
  image renderers) and `reconcile` (line diff → minimal `replaceString`
  patches inside a write command; re-apply highlighters/guards only on
  patched lines; full re-apply fallback).
- [ ] **Step 3:** `DocxFileEditor`/`DocxEditorProvider`; platform test: open
  fixture → editor text contains the document text; guarded ranges reject a
  programmatic insert (`assertThrows` on guarded write); image doc creates
  renderers.
- [ ] **Step 4:** `./gradlew build` green; `runIde` smoke (rendering, theme,
  find with Ctrl+F, zoom).
- [ ] **Step 5: Commit** — `"offxy-jetbrains: the IntelliJ editor as the document surface"`

---

### Task 5: `EditPipeline` — native edits replay into the engine

> **Status: DONE** (commit `2d60ed4`), except the `runIde` manual pass. The
> property test earned its keep immediately: it caught a line-diff off-by-one
> in reconcile (dropped newline at patch boundaries); fixed by switching to
> char-level common prefix/suffix. Deterministic + random suites green.

**Files:**
- Create: `editor/EditPipeline.kt`
- Create: `src/test/kotlin/dev/yeroo/offxy/editor/EditPipelinePropertyTest.kt`
- Modify: `editor/EditorView.kt`, `editor/DocxFileEditor.kt`

**Interfaces:**
- Consumes: `DocumentListener` (beforeDocumentChange for removed range
  capture, documentChanged for replay), Task 4's offset mapping, engine
  commands (`click`, `insert`, `newline`, `paste`, plus selection-delete via
  `click`+`click …\t1`+`delete`).
- Produces: the one edit pathway — typing, backspace/delete, Enter, native
  paste/cut, native undo/redo all arrive as DocumentEvents and replay in
  order: map (offset → line/col via the PRE-EDIT ViewModel), sync position,
  apply removal then insertion (multi-line insertion = `paste`), then
  `reconcile` with the engine's returned view. Engine work runs on a
  per-editor sequential queue off the keystroke path (EDT-dispatched engine
  calls, coalesced reconciles); `suppressListener` guards self-writes.
  Mapping failure or engine error → full `applyRender` (engine authoritative).
  Dirty flag from each returned view → `PROP_MODIFIED`.

- [ ] **Step 1:** Implement the listener + replay + queue.
- [ ] **Step 2: Property test (the crux, headless, no IDE):** a plain
  `DocumentImpl` + real `ChicoryEngine`; hundreds of random edit steps
  (insert char/string/newline at random editable offsets, delete random
  in-seg ranges) applied to the Document with the pipeline attached; after
  every step, Document text == engine render text. Runs seeded (repeatable);
  failures print the seed + script.
- [ ] **Step 3: Platform tests:** typing updates engine (render contains the
  char) and fires modified; Enter splits a paragraph; native undo restores
  BOTH Document and engine text; paste of multi-line text lands as one
  engine paste.
- [ ] **Step 4:** `runIde` manual: type on complex0.docx — native feel, async
  catch-up, no divergence after a burst of fast typing.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: native edits replay into the engine, reconciled"`

---

### Task 6: Formatting, snapshot undo, save, toolbar, markdown

**Files:**
- Create: `editor/Formatting.kt`, `editor/DocxToolbar.kt`, `actions/*.kt`, `editor/EmptyDocPanel.kt`
- Modify: `plugin.xml`, `editor/DocxFileEditor.kt`

**Interfaces:**
- Consumes: webview.js's toolbar table + extension.ts's COMMANDS list
  (the button/command set being mirrored), `UndoManager` +
  `BasicUndoableAction` + `DocumentReferenceManager`, `WriteAction`,
  VFS listeners, `FileDocumentManagerListener`.
- Produces: formatting flow = sync editor selection into the engine
  (`click` + shift-`click`), dispatch (`bold`/`heading\tN`/`list\t…`/
  `align\t…`/`fontsize\t±2`/`replace\t…`…), reconcile, and register ONE
  `BasicUndoableAction` holding before/after `engine.save()` snapshots
  (undo/redo = `open(bytes)` + `applyRender`). Toolbar + plugin.xml actions
  (enabled on focused Offxy editor); save/Save All/close-confirm via
  `WriteAction` writes of `save()` bytes; external-change reload prompt;
  0-byte create flow (`fromMarkdown("")`); markdown convert/export actions;
  replace dialog. Platform find needs no code.

- [ ] **Step 1:** Formatting + snapshot undo; platform test: bold on a
  selection re-renders styled span, platform undo restores pre-bold render
  AND unmodified text.
- [ ] **Step 2:** Save/reload/empty-file/dirty wiring; platform test: save
  round-trip; external change prompt path unit-shaped.
- [ ] **Step 3:** Toolbar + actions + markdown + replace; smoke via `runIde`.
- [ ] **Step 4: Commit** — `"offxy-jetbrains: formatting with snapshot undo, save, toolbar, markdown"`

---

### Task 7: Agent ctl bridge

**Files:**
- Create: `ctl/CtlServer.kt`, `ctl/Discovery.kt`
- Modify: `editor/DocxFileEditor.kt`, `plugin.xml` (appLifecycleListener if needed for shutdown cleanup)

**Interfaces:**
- Consumes: `docs/agent-control.md`; agent-access plan Task 4's server
  behavior (token-first, id echo, single in-flight queue, 30 s discovery
  refresh); `DocxEngine.ctl`.
- Produces: per open editor a loopback ctl server + discovery file
  `docxy-jetbrains-<sanitized basename>-<seq>` in docxy's ctl dir; host verbs
  (`doc.save/reload/open/path`) answered in Kotlin; other `doc.*` →
  `engine.ctl` on the EDT (`invokeLater` + future, 10 s timeout), or the
  conformant `{"ok":false,"error":"not yet implemented"}` while `ctl` probes
  null; mutating verbs reconcile + snapshot-undo like formatting commands;
  cleanup on dispose/shutdown; sweep-resilient refresh.

- [ ] **Step 1:** Server + discovery with fake-engine unit tests (bad token,
  id echo, unknown verb, file shape, refresh timer, dispose).
- [ ] **Step 2:** Editor wiring + real-TCP integration test against a real
  `ChicoryEngine` (host verbs faked; doc verbs per artifact state).
- [ ] **Step 3:** `runIde` manual: `docxy_list` from a `docxy --mcp` session
  shows the instance; live edit round-trip once `docx_ctl` lands.
- [ ] **Step 4: Commit** — `"offxy-jetbrains: open tabs advertise on the agent control surface"`

---

### Task 8: Full verification, CI, docs

**Files:**
- Modify: `.github/workflows/` (gradle job + release asset), root `README.md`, `docs/agent-control.md` (JetBrains subsection)
- Create: `offxy-jetbrains/README.md`, `offxy-jetbrains/CHANGELOG.md`

- [ ] **Step 1:** All Rust gates + full `./gradlew clean build buildPlugin`;
  property test rerun with a fresh seed batch.
- [ ] **Step 2:** CI job (JDK 17 + rust wasm target + gradle, cached);
  verify on a branch push with `gh run watch`.
- [ ] **Step 3:** Docs: install (from-disk zip), features, AI-assistant
  section, known limits (continuous flow, WMF/EMF fallback, no xlsx yet).
- [ ] **Step 4:** Manual e2e checklist for Boris: corpus docs incl.
  tables/lists/images; typing feel on complex0.docx; TUI round-trip; themes;
  Claude Code live-tab edit; terminal + IDE `target` disambiguation.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: CI, docs, verification pass"`

## Self-Review Notes

- Spec coverage after revision: editor surface → Task 4; edit replay +
  reconciliation (the crux, property-tested) → Task 5; formatting/undo/save →
  Task 6; `segs` (the one Rust change, replacing the dropped `find` op) →
  Task 3; ctl → Task 7; packaging → Tasks 1 + 8.
- Perf gate: retired by architecture (async catch-up), benchmark retained as
  the catch-up metric; windowed render remains a listed follow-up, not v1.
- Undo ownership is singular: platform Document undo for text (replayed like
  any edit), snapshot `UndoableAction`s for engine commands; the engine's
  internal undo stack is deliberately unused (interleaving hazard).
- Known judgment calls encoded: engine-authoritative reconciliation as the
  universal failure fallback; guarded blocks from `segs` complements;
  multi-caret out of scope.
