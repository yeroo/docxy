# Offxy JetBrains Plugin (native docx editor) — Design

**Date:** 2026-07-21
**Status:** Approved (design review with Boris, this session). **Revised same
day** after Tasks 1–2: rendering moved from a custom-painted panel to the
IntelliJ editor itself, with a **live editable Document** the engine follows
(both revisions Boris's). The engine layer (Tasks 1–2, committed) is
unchanged by the revision.

## Summary

A **native** JetBrains IDE plugin named **Offxy** that edits Word `.docx`
files in an IntelliJ editor tab — no JCEF, no webview, and no custom text
renderer either: the document is shown in a real IntelliJ `Editor` over an
**editable** `Document`. The engine's grid render (styled monospace lines from
the TUI render engine) is the document text; span styling is `RangeHighlighter`
attributes; decorations (list markers, table borders, image regions) are
**guarded blocks**. The user edits the Document natively — native typing
latency, caret, selection, IME, find, themes — while a `DocumentListener`
replays each change into the engine, whose authoritative re-render reconciles
the Document by minimal diff (a no-op in the common case).

The engine is the *same* `docxwasm.wasm` artifact the VS Code extension ships,
executed on the JVM by **Chicory** (pure-Java wasm runtime, no native code).
Docx ships in v1; the xlsx grid editor is designed-for but not built. Every
open tab advertises on the ctlcore agent-control protocol, so Claude Code and
Junie can read and edit live documents exactly as they do terminal docxy panes
and VS Code tabs.

## Decisions made during review

- **Native, not JCEF** (Boris). Cheap because the webview was never a
  rich-text editor: `docxwasm::Session` renders styled monospace lines — the
  UI is a text presenter, not a word processor.
- **Docx first, xlsxy later** (Boris). Marketplace survey: xlsx has several
  viewer plugins; docx editing has only Syncfusion's commercial JCEF plugin.
- **Reuse the IntelliJ editor, don't paint** (Boris). The grid view is
  monospace text with uniform cell size — exactly the code editor's model.
  Precedent: `ConsoleViewImpl` (an editor with managed content). Free:
  rendering performance, caret/selection/mouse, Ctrl+F find, IME,
  accessibility, themes, zoom.
- **Editable Document, engine follows** (Boris; "why readonly — we can do
  better"). Typing goes into the Document natively and is replayed into the
  engine, rather than intercepted and engine-applied first. This makes felt
  typing latency native and independent of document size — which retires the
  Task 2 performance-gate concern (engine sync is catch-up work off the
  critical path; complex0.docx's ~60 ms is absorbed asynchronously).
- **Chicory, not JNI/Panama cdylibs.** Zero imports + manual ABI is Chicory's
  ideal case; one artifact shared with the VS Code extension. Benchmarked in
  Task 2: 0.35 ms (small) / 6.7 ms (typical 20 KB) / 62 ms (220 KB stress
  doc) per full render — acceptable as async catch-up. The `DocxEngine`
  interface remains the seam for a Panama fallback; the windowed-render
  protocol improvement stays on the follow-up list (benefits VS Code too).
- **One Rust change:** `view_json` additionally serializes the per-line
  editable segment column ranges it already computes (`LineMap` segs). The
  host needs them for guarded-block placement and exact offset→model mapping.
  (The earlier plan's `find` op is dropped — platform find works on the
  Document for free.)
- **Undo is platform-owned.** Text edits undo as native Document undo (the
  undo's DocumentEvent replays into the engine like any other edit). Engine
  commands that change formatting (bold, heading, lists…) register an
  `UndoableAction` holding before/after engine save-bytes snapshots — restore
  is `open(bytes)` + full re-render. The engine's internal undo stack is not
  used (it would interleave wrongly with replayed edits).
- **Same namespaces for agents:** tabs advertise as
  `docxy-jetbrains-<basename>-<n>` in docxy's ctl dir; `docxy --mcp` sees
  them with zero reconfiguration. No new MCP server (JetBrains AI
  Assistant/Junie register `docxy --mcp`).
- **Defaults:** module `offxy-jetbrains/` in this repo (standalone Gradle
  build), Kotlin + IntelliJ Platform Gradle Plugin 2.x, min platform 2024.2,
  GitHub-release distribution first, Marketplace later.

## Structure

```
offxy-jetbrains/
  build.gradle.kts             IntelliJ Platform Gradle Plugin 2.x, Kotlin
                               (Task 1, committed: wasm via processResources)
  src/main/kotlin/dev/yeroo/offxy/
    engine/DocxEngine.kt       the engine seam (Task 2, committed)
    engine/ChicoryEngine.kt    Chicory binding (Task 2, committed)
    editor/ViewModel.kt        parse view JSON: lines/spans/caret/dirty/images/segs
    editor/DocxEditorProvider.kt  FileEditorProvider for *.docx
    editor/DocxFileEditor.kt   FileEditor shell: lifecycle, isModified, save
    editor/EditorView.kt       IntelliJ Editor + Document ownership, render →
                               document text + highlighters + guards + image
                               renderers, minimal-diff reconciliation
    editor/EditPipeline.kt     DocumentListener → engine commands (position
                               sync + insert/delete/paste), async engine sync,
                               reconcile scheduling, self-write guard
    editor/Formatting.kt       engine-command actions + snapshot undo
    editor/DocxToolbar.kt      ActionToolbar (same buttons as the webview)
    actions/                   convertMarkdown / exportMarkdown / replace
    ctl/CtlServer.kt           loopback TCP + discovery files (ctlcore wire)
```

## Engine binding (Chicory) — built, Task 2

As designed: marshalling ports `webview.js` (alloc → call → length-prefixed
result → free); one Chicory instance per document; EDT-confined; runtime
bytecode compiler backend; `docx_ctl` probed and nullable until the
agent-access artifact lands; stateless markdown⇄docx exposed.

## Editor view — the editable-Document architecture

**Render → Document.** The engine view JSON becomes: document text = rendered
lines joined by `\n`; per-span `TextAttributes` on `RangeHighlighter`s
(bold/italic via font type, underline/strike effects, dim alpha, ANSI color
names mapped through the scheme's console palette, engine `h` flags unused —
selection is the editor's own); image boxes drawn by custom renderers over
the grid rows the render reserves (`EditorCustomElementRenderer` /
`CustomHighlighterRenderer`), bytes via `docx_media` + `ImageIO`, WMF/EMF →
labeled fallback box. Grid ↔ editor mapping is the identity: engine
`(line, col)` = `LogicalPosition(line, col)` (the render emits spaces, no
tabs).

**Guarded blocks.** The new `segs` field in the view JSON gives, per line, the
column ranges that are real model text. Everything outside them — list
markers, table borders, image regions, blank structural lines — is covered by
`Document.createGuardedBlock`, so native editing physically can't land there;
platform actions that would cross a guard fail gracefully (the GUI-designer
precedent).

**Native edit → engine replay.** A `DocumentListener` receives each user edit
as (offset, removed, inserted). `EditPipeline` translates: map offset through
segs to the engine position, sync (`click`), reconstruct removals as
selection (`click` + `click …\t1`), then `insert`/`delete`/`newline`/`paste`
commands. Multi-line insertions become `paste`. Edits arrive from: typing,
backspace/delete, Enter, native paste/cut, native undo/redo — one pathway for
all of them, no per-action handlers.

**Reconciliation.** After the engine applies a replayed edit (or any engine
command), its fresh render is diffed line-wise against the Document and only
changed regions are `replaceString`-patched inside a write action flagged as
self-inflicted (the listener ignores self-writes). Common case — edit without
re-wrap — the diff is empty. Re-wrap/marker changes patch exactly the lines
that visibly changed. On any mapping failure or engine error the full render
is re-applied: **the engine is always authoritative**; the worst bug class is
a visible correction, not silent divergence. Engine sync runs async off the
EDT keystroke path (queued, in-order, coalesced); the Document stays
responsive at native speed on any document size.

**Caret.** Editor-owned. The engine's caret only matters inside command
handling, set by the sync `click` preceding each replayed edit. After
reconciliation patches, the platform's standard offset-shifting keeps the
editor caret in place.

**Width sync.** Wrap width follows the editor's visible column count
(`width\t<cols>`, min 20, debounced on resize/zoom/font change) — a re-render
+ reconcile pass.

**Empty file.** 0-byte `.docx` → in-tab "Create new Word document" panel;
Create mints `fromMarkdown("")` bytes into the file (extension.ts's
`mintEmpty` flow).

## Formatting commands, dirty state, save

- **Formatting** (bold/italic/underline/strike, headings, lists, alignment,
  indent, font size, color, replace-all): sync the engine selection from the
  editor selection, dispatch the command, reconcile, and register one
  `UndoableAction` whose undo/redo restore before/after engine snapshots
  (`save()` bytes — ~10 ms on the stress doc, rare operations). Exposed as an
  `ActionToolbar` over the editor plus plugin.xml actions (Find Action +
  keymap), enabled when an Offxy editor is focused.
- **Dirty flag:** view JSON `dirty` → `FileEditor.isModified` +
  `PROP_MODIFIED` events.
- **Save:** `docx_save` bytes → `VirtualFile` in a `WriteAction`; hooked into
  Save All / Ctrl+S and close-with-confirmation. External disk change → reload
  prompt → fresh engine open + full re-render.
- **Markdown ⇄ docx:** project-view action on `.md` → sibling `.docx`; export
  action → sibling `.md` (stateless engine conversions).
- **Find:** the platform's editor find over the Document — zero code.
  Replace-all goes through the engine (`replace\t…`) like other formatting.

## Agent ctl bridge

Unchanged from the original design: per open tab a Kotlin ctl server speaking
ctlcore's wire (loopback TCP, token, discovery file
`docxy-jetbrains-<sanitized basename>-<seq>`, stale-sweep-resilient refresh);
host verbs (`doc.save/reload/open/path`) answered in Kotlin; doc verbs through
`DocxEngine.ctl` (the `docx_ctl` export from the agent-access plan) with the
conformant `not yet implemented` reply until that artifact lands; requests hop
to the EDT with a timeout, one in flight per document. Mutating ctl verbs
reconcile the Document and register the same snapshot `UndoableAction` as
formatting commands. Security posture identical to PR #19.

## Packaging & release

As built in Task 1: standalone Gradle module; wasm flows through
`processResources { from(buildWasm) }` with cargo staleness tracking; plugin
zip carries Chicory (+ its shaded ASM) only; since-build 242; GitHub release
assets; CI job added at the end (Task 8); Marketplace deferred.

## Testing

- **Engine tests (Task 2, committed):** open/render/edit/undo/save round-trip/
  media/markdown against real fixtures, plus the benchmark (now
  informational: it measures async catch-up latency, not felt keystrokes).
- **Rust:** the `segs` addition to `view_json` — native tests asserting
  segment ranges for plain, wrapped, listed, and table paragraphs.
- **Mapping property test (the crux):** headless, no IDE — random edit
  scripts (insert/delete/enter at random valid positions) applied to both a
  plain-text shadow model and through the offset-mapping + replay + reconcile
  pipeline against the real engine; after every step the Document text must
  equal the engine's render. Hundreds of iterations; this is the test that
  guards against silent divergence.
- **Platform tests (`BasePlatformTestCase`):** open a fixture → editor shows
  rendered text; typing in an editable region updates both Document and
  engine (dirty fires); typing into a guarded region is rejected; native undo
  restores text and engine agrees; formatting action + undo restores
  attributes; save bytes reopen with the edit.
- **Ctl integration:** as before (TCP round-trip against a real engine;
  full verb set once `docx_ctl` lands).
- **Manual e2e:** `runIde` on corpus docs (tables/lists/images); typing feel
  on complex0.docx (the async-catch-up check); TUI round-trip fidelity;
  theme switch; Claude Code editing a live tab; terminal + IDE instances
  disambiguated by `target`.

## Out of scope (v1)

- Xlsx grid editor (designed-for; second registration).
- Compose/Jewel; JCEF.
- Marketplace publishing.
- WMF/EMF/SVG rasterization (fallback box).
- Page view/pagination (continuous flow, as VS Code).
- Multi-caret / column-selection editing (guards + single-caret replay only;
  extra carets are allowed to exist but edits replay sequentially).
- JetBrains remote development / Gateway thin-client support.
