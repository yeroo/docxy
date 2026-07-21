# Offxy JetBrains Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A native (Swing, no JCEF) JetBrains plugin that edits `.docx` in an IDE tab — same `docxwasm.wasm` engine as the VS Code extension, run on Chicory — with IDE-native undo/save/theming and per-tab agent ctl access.

**Architecture:** `offxy-jetbrains/` is a standalone Gradle build (Kotlin, IntelliJ Platform Gradle Plugin 2.x) inside this repo. `ChicoryEngine` marshals the docxwasm ABI (alloc → call → read `[u32 le len][payload]` → free) behind a `DocxEngine` interface, one Chicory instance per open document, all calls EDT-confined. `DocPanel` ports `webview.js`'s grid painting (spans at `col × charW`, caret rect, engine-side selection, ImageIO image overlays); `DocxFileEditor` provides undo lockstep (`BasicUndoableAction` per mutating command), dirty state, and `WriteAction` saves. `CtlServer` ports the agent-access plan's Task 4 server to Kotlin, advertising `docxy-jetbrains-<basename>-<n>` in docxy's ctl dir.

**Tech Stack:** Kotlin/JVM 17, IntelliJ Platform Gradle Plugin 2.x (target IC 2024.2 / since-build 242), Chicory (only runtime dep), Rust (one small docxwasm addition).

**Spec:** `docs/superpowers/specs/2026-07-21-offxy-jetbrains-design.md`
**References:** `offxy-vscode/media/webview.js` (the painting/input contract being ported), `offxy-vscode/src/extension.ts` (host duties), `docxwasm/src/lib.rs` (ABI), `docs/agent-control.md` + `docs/superpowers/plans/2026-07-17-offxy-agent-access.md` (ctl wire + `docx_ctl`).

## Global Constraints

- **No behavior change** to the terminal apps or the VS Code extension. The only Rust change is Task 3's `find` dispatch op (additive; all existing tests stay green).
- Workspace version stays put; no bumps — release is Boris's call.
- `offxy-jetbrains/` is NOT a cargo workspace member and the Gradle build must not touch the cargo workspace beyond invoking `cargo build -p docxwasm --target wasm32-unknown-unknown --release`.
- Runtime dependency rule: **Chicory only** (com.dylibso.chicory, latest stable on Maven Central; runtime + AOT modules). Verify its current API from its docs/javadoc before writing `ChicoryEngine` — do not guess class names.
- Wire compatibility is sacred for Task 7: EXACTLY ctlcore's protocol (one JSON object per line; `{"token","verb","args","id?"}` → `{"ok":true,"result":…,"id?"}` / `{"ok":false,"error":…}`); discovery files `{"instance","port","token","pid"}` in `%APPDATA%\docxy\ctl` / `$XDG_CONFIG_HOME/docxy/ctl`.
- All Rust gates: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -p docxwasm`. Gradle gates: `./gradlew build` (compiles + tests) and `./gradlew buildPlugin`.
- **JDK:** requires JDK 17+. If `java -version` fails, install via `scoop install temurin17-jdk` (or current LTS bucket name) before Task 1.
- **Windows agent shell quirks:** every cargo command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Gradle runs via `./gradlew` from `offxy-jetbrains/`.

---

### Task 1: Scaffold the Gradle plugin project

> **Status: DONE** (commit `3981624`). JDK 17 (temurin) + Gradle installed via
> scoop; wasm32-unknown-unknown target had to be `rustup target add`ed. The
> wasm ships into resources via `processResources { from(buildWasm) }` — a
> Copy task into `src/main/resources` trips Gradle's implicit-dependency check
> against `patchPluginXml`.

**Files:**
- Create: `offxy-jetbrains/build.gradle.kts`, `settings.gradle.kts`, `gradle.properties`, wrapper files, `src/main/resources/META-INF/plugin.xml`, `.gitignore` (build/, .gradle/, the copied wasm)

**Interfaces:**
- Produces: a building, installable (empty) plugin — id `dev.yeroo.offxy`, name "Offxy", since-build `242`, no until-build; a `copyWasm` Gradle task that copies `../target/wasm32-unknown-unknown/release/docxwasm.wasm` into `src/main/resources/` (running the cargo build first when the artifact is missing or older than the docxwasm/docxcore sources), wired as a dependency of `processResources`; Chicory on the runtime classpath.

- [ ] **Step 1:** Generate the Gradle scaffold (wrapper via a locally installed gradle or the init task; IntelliJ Platform Gradle Plugin 2.x `intellijPlatform` DSL targeting IC 2024.2). Kotlin jvmToolchain(17).
- [ ] **Step 2:** `plugin.xml` with id/name/vendor/description; no extensions yet.
- [ ] **Step 3:** `copyWasm` task + Chicory dependency.
- [ ] **Step 4: Verify** — `./gradlew buildPlugin` produces `build/distributions/offxy-*.zip` containing `docxwasm.wasm` and the Chicory jars. Report the zip listing.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: Gradle scaffold — IntelliJ plugin skeleton with the docxwasm artifact"`

---

### Task 2: `DocxEngine` + `ChicoryEngine`, tests, and the performance gate

> **Status: DONE, GATE MISSED on the largest doc** (commit `e4a2c6a`). All
> engine tests green. Benchmark: complex0.docx (220 KB) p50 60 ms / p95
> 62.6 ms — over the 50 ms line, and steady-state (500-iteration warmup
> changes nothing). But latency is linear in document size: 20 KB
> sample.docx is 6.7 ms, small docs 0.35 ms. Root cause is the protocol
> (full-document render+serialize per keystroke), not Chicory overhead.
> STOPPED per gate for Boris's call: accept / Panama FFI / windowed render.

**Files:**
- Create: `src/main/kotlin/dev/yeroo/offxy/engine/DocxEngine.kt`, `engine/ChicoryEngine.kt`
- Create: `src/test/kotlin/dev/yeroo/offxy/engine/ChicoryEngineTest.kt`, `engine/EngineBenchmark.kt` (a test, reported not asserted)

**Interfaces:**
- Consumes: `docxwasm/src/lib.rs` (read FULLY — the ABI contract lives in its doc comments), `offxy-vscode/media/webview.js` lines 28–106 (the exact marshalling being ported).
- Produces (Tasks 4–7 depend on these exact signatures):
  ```kotlin
  interface DocxEngine : AutoCloseable {
    fun open(bytes: ByteArray): Boolean          // docx_open; false if unparseable
    fun render(): String                         // docx_render → view JSON
    fun cmd(command: String): String             // docx_cmd → view JSON
    fun save(): ByteArray                        // docx_save
    fun media(rid: String): ByteArray            // docx_media (empty if unknown)
    fun ctl(requestJson: String): String?        // docx_ctl if exported, else null
    companion object {
      fun fromMarkdown(md: String): ByteArray    // stateless docx_from_markdown
      fun toMarkdown(bytes: ByteArray): String   // stateless docx_to_md
    }
  }
  ```
  `ChicoryEngine` = one Chicory instance per `DocxEngine` (fresh module instantiation in the constructor; `close()` drops it). NOT thread-safe by contract — callers confine to the EDT (Task 4). `ctl` probes the export table once and returns null when `docx_ctl` is absent (today's artifact — it arrives with the agent-access plan).
- View JSON parsing: add a tiny `ViewModel.kt` later (Task 4) — engine returns raw strings.

- [ ] **Step 1: Write failing tests** against real fixtures (copy 2–3 small docs from `corpus/` or `assets/` into `src/test/resources/`, including one with an embedded image): open+render contains the text and `"caret"`; `cmd("insert\tX")` returns `"dirty":true` and the text; save round-trip reopens with the edit (mirror `save_round_trips_edit` in bridge.rs); `media` returns non-empty bytes for the image doc's rid (parse rid from the view's `images` array); `fromMarkdown("# T")`/`toMarkdown` round-trip; `cmd("undo")` after an insert restores.
- [ ] **Step 2: RED**, then implement `ChicoryEngine` (alloc/write/call/read-length-prefixed/free, exactly webview.js's sequence — re-fetch the memory view after alloc). Use Chicory's AOT/compiler backend if the current API offers it cheaply; interpreter otherwise.
- [ ] **Step 3: GREEN** — `./gradlew test`.
- [ ] **Step 4: Benchmark.** Open the LARGEST `.docx` in `corpus/` (find it by size); measure warm per-call time of `cmd("insert\ta")` + `cmd("backspace")` pairs and `cmd("width\t120")` re-render, 200 iterations after 20 warmup. Print p50/p95. **Gate:** p95 ≤ 50 ms → proceed; else STOP and report — the Panama-FFI fallback becomes a design conversation with Boris, not an improvisation.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: ChicoryEngine — the docxwasm ABI on the JVM, benchmarked"`

---

### Task 3: `find` dispatch op in docxwasm (Rust)

**Files:**
- Modify: `docxwasm/src/bridge.rs`

**Interfaces:**
- Consumes: how docxy's TUI find moves the caret (`docxy/src/` — locate its find implementation and mirror semantics: case-insensitive, wraps past the end, selects the match).
- Produces: dispatch op `find\t<query>` — from the caret, move to the next occurrence, select it (caret at match end, selection covering it); no match → no movement, not dirty. Navigation-class op (`mutated = false`).

- [ ] **Step 1: Failing tests** in bridge.rs's test style: `find_moves_caret_and_selects` (two occurrences: two finds land on each in order, `"selection":1` in the view), `find_wraps_around`, `find_no_match_is_noop_and_clean` (not dirty).
- [ ] **Step 2: Implement** inside `Session::dispatch`'s match (reuse the editor/find machinery the TUI uses; do not invent a new search).
- [ ] **Step 3:** `cargo test -p docxwasm` green; fmt/clippy; wasm32 release build; re-run Task 2's Kotlin tests against the fresh artifact (`copyWasm` picks it up) with one new test: `cmd("find\tworld")` sets `"selection":1`.
- [ ] **Step 4: Commit** — `"docxwasm: find command — next-match navigation for hosts without a DOM find widget"`

---

### Task 4: `DocPanel` rendering + provider skeleton

**Files:**
- Create: `editor/ViewModel.kt` (parse the view JSON: lines/spans with b/i/u/s/d/h/c/lnk, caret, dirty, images), `editor/DocPanel.kt`, `editor/DocxFileEditor.kt`, `editor/DocxEditorProvider.kt`
- Modify: `plugin.xml` (register `fileEditorProvider`)

**Interfaces:**
- Consumes: Task 2's `DocxEngine`; `webview.js` lines 108–245 (images, paint, caret, metrics, width sync — the porting contract); IntelliJ: `FileEditorProvider`/`FileEditor`, `EditorColorsManager` (global scheme font + colors), `JBScrollPane`, `ConsoleViewContentType`'s ANSI color keys for the span-color mapping (`color_name` in bridge.rs lists the 16 names).
- Produces: opening a `.docx` shows the rendered document — spans at `col × charW` (grid-aligned, never advance-by-string-width), bold/italic via `font.deriveFont`, underline/strike rules, dim alpha, `h` spans in the scheme's selection background, editor background, caret rect with blink timer, images via `ImageIO` with the labeled fallback box (PNG/JPEG/GIF/BMP sniffing like `sniffMime`; cache per rid), status line `Ln, Col · N lines · ●`. Width syncs to viewport (`width\t<cols>`, min 20, debounced); font/LAF change listeners remeasure + repaint. Provider `accept` = filename ends `.docx` (policy `HIDE_DEFAULT_EDITOR`); dispose closes the engine.

- [ ] **Step 1:** `ViewModel.kt` + unit tests parsing captured view JSON (grab real output from a Task 2 test fixture).
- [ ] **Step 2:** `DocPanel` painting + `DocxFileEditor`/`DocxEditorProvider` lifecycle (engine per editor, EDT-only).
- [ ] **Step 3: Platform test** (`BasePlatformTestCase`): copy a fixture into the test VFS, open via `FileEditorManager`, assert our editor is selected and its panel's view model has >0 lines containing the fixture text. (Headless: assert the model, not pixels.)
- [ ] **Step 4:** `./gradlew build` green; `./gradlew runIde` smoke — open a corpus doc, confirm rendering/scrolling/theme by eye (report screenshots optional).
- [ ] **Step 5: Commit** — `"offxy-jetbrains: native document rendering — the grid painter over docxwasm"`

---

### Task 5: Input, editing, undo lockstep, dirty state, save

**Files:**
- Modify: `editor/DocPanel.kt` (input), `editor/DocxFileEditor.kt` (undo/dirty/save)
- Create: `editor/EmptyDocPanel.kt` (create-new flow)

**Interfaces:**
- Consumes: `webview.js` lines 247–349 (the key/mouse table and `MUTATING` set — port them faithfully), `extension.ts`'s `edit`/save/revert/mintEmpty flows (the semantics being mirrored); IntelliJ: `UndoManager` + `BasicUndoableAction` + `DocumentReferenceManager.create(virtualFile)`, `WriteAction`, `CopyPasteManager`, VFS change listener, `FileDocumentManagerListener` (save-all hook).
- Produces: typing/arrows/word-moves/home-end/doc-moves with Shift extension, Enter/Backspace/Delete/Tab, Ctrl+B/I/U, Ctrl+A/C/X/V through the OS clipboard, click/drag selection, Ctrl+click links → `BrowserUtil.browse`. Each mutating command registers ONE `BasicUndoableAction` whose undo/redo dispatch `undo`/`redo` — platform Ctrl+Z must reach it (verify routing via the platform test; `FileEditor.getFile` is implemented). `dirty` from the view drives `isModified` + `PROP_MODIFIED`. Save = `docx_save` bytes → `VirtualFile` in a `WriteAction`, hooked into Save All / Ctrl+S (FileDocumentManagerListener's beforeAllDocumentsSaving + an editor-tab save on frame deactivation per IDE convention) and prompted on close-with-modifications. External disk change while open → reload prompt → fresh engine open. A 0-byte file shows `EmptyDocPanel` with a Create button minting `fromMarkdown("")` into the file.

- [ ] **Step 1:** Port the input table + clipboard + mouse (mod-key mapping: Ctrl on Win/Linux, Cmd on mac via `SystemInfo`).
- [ ] **Step 2:** Undo/dirty/save/reload/empty-file wiring.
- [ ] **Step 3: Platform tests:** typing marks modified and fires PROP_MODIFIED; platform Undo action restores the text AND clears modified (undo reaches the engine — this test is the routing proof); save writes bytes that `DocxEngine` reopens with the edit; empty file shows the create panel.
- [ ] **Step 4:** `runIde` manual pass: edit/undo/redo/save a corpus doc, reopen in the TUI (`cargo run -p docxy -- <file>`) to confirm fidelity.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: editing — input table, undo lockstep, dirty state, save"`

---

### Task 6: Toolbar, actions, find bar, markdown, replace

**Files:**
- Create: `editor/DocxToolbar.kt`, `editor/FindBar.kt`, `actions/ConvertMarkdownAction.kt`, `actions/ExportMarkdownAction.kt`, `actions/ReplaceAction.kt`
- Modify: `plugin.xml` (action registrations + project-view group), `editor/DocxFileEditor.kt`

**Interfaces:**
- Consumes: `webview.js`'s toolbar button table (lines 398–444), `extension.ts`'s COMMANDS list + convert/export/replace flows; Task 3's `find` op.
- Produces: an `ActionToolbar` above the document (B/I/U/S, H1/H2/¶, bullet/number, align, A−/A+ — each dispatches its command and refocuses the panel); the same actions as `AnAction`s in plugin.xml (Find Action + keymap-bindable), enabled only when an Offxy editor is focused. Ctrl+F toggles `FindBar` (a `SearchTextField`; Enter = `find\t<query>`, Esc closes and refocuses). Project-view action on `.md` → sibling `.docx` via `fromMarkdown` + open in Offxy; export action → sibling `.md` via `toMarkdown` + open. Replace = two `Messages.showInputDialog`s → `replace\t<find>\t<with>` (a mutating command — one undo step, from Task 5's plumbing).

- [ ] **Step 1–3:** Implement toolbar → actions → find bar → markdown/replace, with platform tests for: an action dispatches and registers undo; convert produces a `.docx` that opens; find selects the match.
- [ ] **Step 4:** `./gradlew build` green; `runIde` smoke.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: toolbar, palette actions, find bar, markdown conversion"`

---

### Task 7: Agent ctl bridge

**Files:**
- Create: `ctl/CtlServer.kt`, `ctl/Discovery.kt`
- Modify: `editor/DocxFileEditor.kt` (server lifecycle), `plugin.xml` if a shutdown hook needs an appLifecycleListener

**Interfaces:**
- Consumes: `docs/agent-control.md` (wire + verb contracts), agent-access plan Task 4 (the TS server whose behavior this ports: token-first check, id echo, single in-flight queue, 30 s discovery-refresh timer), Task 2's `DocxEngine.ctl`.
- Produces: per open docx editor, a loopback `ServerSocket(0)` speaking ctlcore's wire; discovery file `docxy-jetbrains-<sanitized basename>-<seq>` (sanitize like the TS server: lowercase, non-alnum → `-`) in docxy's ctl dir; deleted on editor dispose + app shutdown, re-written by the refresh timer if swept. Verb routing: `doc.save`/`doc.reload`/`doc.open`/`doc.path` answered by the host (save via Task 5's save path; reload = fresh open; open = `FileEditorManager.openFile`; path = file path + dirty + block count from a ctl `doc.blocks` call when available). All other `doc.*` → `DocxEngine.ctl` on the EDT (`invokeLater` + future, 10 s timeout); `ctl == null` (no `docx_ctl` export yet) → `{"ok":false,"error":"not yet implemented"}`. Mutating verbs (replace-range/insert/append) register one `BasicUndoableAction` and repaint, exactly like keyboard edits.

- [ ] **Step 1:** `CtlServer` + `Discovery` with unit tests using a fake engine: bad token rejected; id echoed; unknown verb `ok:false`; discovery file shape; refresh timer restores a deleted file (injectable short interval); dispose cleans up.
- [ ] **Step 2:** Wire into the editor lifecycle; integration test over a REAL TCP socket against a REAL `ChicoryEngine` (headless, no IDE: fake the host verbs): `doc.path`-composition, and — once `docx_ctl` exists in the artifact — read → replace-range → undo round-trip. Until then the test asserts the conformant not-implemented reply.
- [ ] **Step 3:** `runIde` manual: with a doc open, `docxy --mcp` from a terminal — `docxy_list` shows `docxy-jetbrains-…`; if `docx_ctl` has landed, a `docxy_replace_range` edit appears live with the dirty asterisk and Ctrl+Z undoes it.
- [ ] **Step 4: Commit** — `"offxy-jetbrains: open tabs advertise on the agent control surface"`

---

### Task 8: Full verification, CI, docs

**Files:**
- Modify: `.github/workflows/` (add a `gradle buildPlugin` job uploading the zip; wire the zip into the release-assets job), `README.md` (repo: mention the plugin), `docs/agent-control.md` ("JetBrains tabs" subsection: `docxy-jetbrains-*` instances, same verbs, `docxy --mcp` registration for AI Assistant/Junie)
- Create: `offxy-jetbrains/README.md`, `offxy-jetbrains/CHANGELOG.md`

**Interfaces:** none new — verification + documentation.

- [ ] **Step 1:** `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -p docxwasm`; both wasm32 builds still green; full `./gradlew clean build buildPlugin`; re-run Task 7's integration test against the final zip's wasm.
- [ ] **Step 2:** CI job (JDK 17 setup + rust target + gradle; cache both). Verify with `gh run watch` on a branch push.
- [ ] **Step 3:** Docs: install instructions (Install Plugin from Disk → the release zip), feature list, AI-assistant section, known limits (continuous flow, WMF/EMF fallback, no xlsx yet).
- [ ] **Step 4:** Manual e2e checklist for Boris (in the report): open corpus docs incl. tables/lists/images; edit/undo/save; TUI round-trip fidelity; light/dark theme; Claude Code live-tab edit; terminal pane + IDE tab disambiguated by `target`.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: CI, docs, verification pass"`

## Self-Review Notes

- Spec coverage: engine binding + gate → Task 2; the one Rust change → Task 3; panel/theme → Task 4; undo/dirty/save/empty → Task 5; toolbar/find/markdown → Task 6; ctl bridge → Task 7; packaging/CI/docs → Task 8. Xlsx: out of scope per spec (the provider seam is the design's registration table).
- Sequencing dependency honored: `docx_ctl` comes from the agent-access plan; Task 7 is conformant without it and lights up on a wasm refresh — no ordering constraint between the two plans.
- Type consistency: `DocxEngine` signatures used in Tasks 4–7 match Task 2; command strings everywhere are `webview.js`'s exact tab-delimited forms plus Task 3's `find`.
- Known judgment calls encoded: EDT confinement (not a lock), one Chicory instance per document, benchmark STOP-gate instead of silently switching to FFI, save-all hook via FileDocumentManagerListener, `HIDE_DEFAULT_EDITOR` policy.
