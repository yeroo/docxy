# Offxy JetBrains Plugin — Test Plan

Covers v1 (native docx editor + agent ctl bridge). Two halves: the automated
suite (run per change, gates CI) and the manual plan (run before merging a
feature branch and before attaching a zip to a release). Known limits at the
end are expected behavior — don't file them as bugs.

## 1. Automated suite (gates every change)

Run:

```sh
cd offxy-jetbrains
./gradlew test --rerun     # --rerun: the build cache replays results otherwise
```

plus the Rust gates when docxwasm changed:

```sh
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings
cargo test -p docxwasm
```

| Layer | Tests | What it proves |
|---|---|---|
| Engine ABI (`ChicoryEngineTest`) | 9 | open/render/edit/undo/save round-trip, media bytes, markdown ⇄ docx, `docx_ctl` probe — the wasm artifact works on the JVM |
| Benchmark (`EngineBenchmark`, `EngineCharacterization`) | 4 | per-keystroke engine latency printed (p50/p95); backstop assert < 500 ms |
| View model (`ViewModelTest`) | 5 | JSON parse (both line shapes), styled ranges, offset↔grid round-trip, guards complement segs |
| Editor surface (`DocxEditorPlatformTest`) | 1 | provider claims `.docx`, renders, guarded columns reject edits |
| Edit pipeline (`EditPipelinePropertyTest`) | 3 | deterministic insert/Enter/delete/paste replay **and the seeded 80-step random-edit property test: Document text ≡ engine render after every step** |
| Formatting & undo (`FormattingPlatformTest`) | 2 | bold on selection, platform undo/redo via snapshots, save round-trip |
| Task-6 flows (`Task6PlatformTest`) | 4 | empty-file create, disk reload, markdown convert/export round-trip, replace-all as one undo step |
| Ctl wire (`CtlServerTest`) | 3 | exact ctlcore error strings, token/id semantics, discovery lifecycle, sweep-resilient refresh |
| Ctl bridge e2e (`CtlBridgePlatformTest`) | 1 | real TCP client against a live editor: `doc.path`/`doc.read`/`doc.save`, undo-verb rejection, `doc.blocks` parity |

On a property-test failure, the message carries `seed=` and the edit script —
replay by seeding `Random(seed)` before shrinking by hand.

## 2. Manual plan

Environment: `./gradlew runIde` (sandbox IDE), or the zip from
`build/distributions/` installed via **Install Plugin from Disk** into a real
2024.2+ IDE. Test documents: `corpus/files/complex0.docx` (large, images,
tables), `assets/sample.docx` (typical), a fresh doc from
`fromMarkdown` (via the `.md` convert action), and any real-world documents at
hand.

### 2.1 Install & load

- [ ] Zip installs into IDEA CE 2024.2+ with no error; plugin listed as
      "Offxy"; IDE restart not required beyond the standard prompt.
- [ ] Also loads in one non-IDEA IDE (PyCharm/WebStorm) — platform-only deps.
- [ ] Opening a `.docx` uses Offxy (no editor-choice tab strip); a `.txt`
      does not.

### 2.2 Rendering

- [ ] `complex0.docx`: text, bold/italic/underline/strike, colors, tables
      (borders aligned), lists (markers), embedded images render; scrolling
      is smooth to the bottom.
- [ ] Theme: switch light ⇄ dark — text, colors, selection all legible, no
      stale artifacts.
- [ ] Editor font size change (Settings or Ctrl+wheel if enabled): grid
      re-measures, wrap width re-syncs, nothing overlaps.
- [ ] Narrow/widen the window: text re-wraps to the viewport (min 20 cols);
      images stay anchored to their rows.
- [ ] Links render underlined; Ctrl+click opens the browser.

### 2.3 Editing (the feel test)

- [ ] Typing in `complex0.docx` feels instant (native latency; the engine
      catch-up ~60 ms must not block keystrokes). Burst-type a full sentence:
      no dropped/reordered chars after the reconcile settles.
- [ ] Typing mid-paragraph near the wrap margin: re-wrap corrects within a
      beat, caret stays put.
- [ ] Enter splits a paragraph; Backspace at line start joins across the
      wrap; Delete forward works; Tab inserts.
- [ ] Click and drag selects; Ctrl+C/X/V round-trip through the OS clipboard
      (paste multi-line text — arrives as separate paragraphs).
- [ ] Typing into a list marker or table border does nothing (guard); typing
      at col 0 of a list item lands at the text start (engine normalizes).
- [ ] Dead keys / IME composition (e.g. `´` + `e` → `é`): composed char
      arrives once.
- [ ] Ctrl+F finds text in the rendered document; F3/Shift+F3 navigate.

### 2.4 Undo / redo

- [ ] Type text → Ctrl+Z restores exactly, engine agrees (text reverts on
      screen after reconcile); Ctrl+Shift+Z / Ctrl+Y redoes.
- [ ] Toolbar bold on a selection → one Ctrl+Z removes it (one step).
- [ ] Interleave: type, bold, type, replace-all → four Ctrl+Z steps unwind
      in order with no text corruption.
- [ ] Undo past the first edit: no-op, no error, tab still consistent.

### 2.5 Dirty, save, reload, fidelity

- [ ] Any edit lights the tab's modified marker; Ctrl+S / Save All clears it
      and writes the file; closing a modified tab saves (auto-save spirit).
- [ ] **TUI round-trip:** edit + save in the IDE → open in terminal `docxy`:
      content and formatting match. Then edit in the TUI, save → IDE tab
      (unmodified) reloads to the new content on focus/VFS refresh.
- [ ] **Lossless check:** open a corpus doc, make one trivial edit, undo it,
      save → `compare` tooling / TUI shows no structural loss (headers,
      styles, numbering, unmodeled parts intact).
- [ ] External change while the tab HAS unsaved edits: tab keeps the user's
      version (last writer wins at next save) — no silent clobber of typing.
- [ ] 0-byte `.docx` (New File): "Create new Word document" button mints a
      valid document; garbage bytes named `.docx`: readable error message,
      no exception balloon.

### 2.6 Actions

- [ ] Toolbar: every button (B/I/U/S, H1/H2/¶, lists, alignment, A−/A+)
      applies, keeps the selection usable, refocuses the document.
- [ ] Project view → `.md` → "Convert Markdown to Word": sibling `.docx`
      opens in Offxy; headings/bold/lists survived.
- [ ] Tools → "Offxy: Export to Markdown": sibling `.md` opens; round-trip
      content matches.
- [ ] Tools → "Offxy: Replace…": replace-all applies; one Ctrl+Z reverts.

### 2.7 Agent bridge (with a `docxy --mcp` session)

- [ ] With one IDE doc open: `docxy_list` shows
      `docxy-jetbrains-<name>-<pid>-<n>` alongside terminal panes.
- [ ] `docxy_read` returns the live (unsaved) text; `docxy_outline` matches
      headings.
- [ ] `docxy_replace_range` repaints the tab live, lights the modified
      marker, and **one Ctrl+Z in the IDE undoes the agent's edit**.
- [ ] `docxy_save` writes the file and clears the marker.
- [ ] Terminal docxy + IDE tab open on different docs: `target` substring
      picks the right one; ambiguous target errors with candidates.
- [ ] Two IDE windows, same basename: two distinct instances (pid differs).
- [ ] `doc.undo` via the bridge: clean rejection ("undo is IDE-owned…").
- [ ] Start a terminal docxy (triggers stale sweep) while the IDE tab is
      open: within ~30 s the tab's discovery file is back and it still
      answers.
- [ ] Close the tab: its discovery file is gone; `docxy_list` no longer
      shows it.

### 2.8 Stability & resources

- [ ] Open 5+ documents at once: each tab independent; close them all — no
      leak balloon, IDE memory returns to baseline (each close drops a wasm
      instance).
- [ ] Leave a tab open 30+ min with the ctl refresh running: no CPU churn.
- [ ] `idea.log` (Help → Show Log): no Offxy exceptions after a full session
      of the above.

## 3. Release checklist (additions)

- [ ] `./gradlew clean buildPlugin` from a clean checkout; zip contains
      `docxwasm.wasm` + Chicory jars only (`unzip -l`).
- [ ] CI green on the release commit (all six jobs).
- [ ] CHANGELOG entry matches the tag; README install link resolves.
- [ ] Fresh-machine smoke: install zip into a stock IDEA CE, open a real
      document, type, save, reopen in Word/LibreOffice.

## 4. Known limits (expected, not bugs)

- Continuous flow — no page boundaries, headers/footers not rendered inline
  (readable via the agent `doc.header`/`doc.footer` verbs).
- WMF/EMF/SVG images: labeled placeholder box (PNG/JPEG/GIF/BMP render).
- Double-width (CJK) columns can drift a beat until reconcile corrects.
- Multi-caret edits are not replayed as simultaneous (single-caret replay);
  column selection may reconcile away.
- `doc.export-pdf` via the bridge: not yet implemented on JetBrains tabs.
- Ctrl+B/I/U default keyboard shortcuts are not bound (IDE keymap conflicts,
  e.g. Ctrl+B = Go to Declaration) — toolbar/Find Action/custom keymap.
