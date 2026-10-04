# Plan v1 for yeroo/docxy#955 — Leftovers from #940 (terminal Design tab)

Issue: `.workbench/issue.md`. Three checklist items: **plan-dialogs**, **plan-pagecolor-view**,
**plan-automation**. This PR does **plan-pagecolor-view** and **plan-automation**. **plan-dialogs**
is deferred to its own follow-up issue (see Out of scope).

## Goal

Terminal docxy's Design tab (#940, PR #954) writes a page colour, a text watermark and page
borders, but (a) Print Layout still draws every page in the terminal's own colours, so a page
colour is invisible except in the status line, and (b) the control pipe (`docxy/src/control.rs`)
and the MCP server (`docxy/src/mcp.rs`, mirrored by `offxy-vscode/mcp/server.mjs`) cannot set any
of the three. After this change Print Layout tints each page sheet with the document's page colour
(ink chosen by luminance, as the suite's `page_ink` does), and three new control verbs / MCP tools
set or remove the page colour, the text watermark and the page borders through the same App code
the ribbon pickers use.

## Acceptance criteria

1. With a page colour set and Print Layout on, every cell of a page's sheet (from its `│`/`┌`
   frame inwards) is drawn with background `Color::Rgb` of the page colour, in both the dark
   (`light_page == false`) and light (`light_page == true`) terminal themes; cells outside the page
   (centering margin, gaps between pages) are `Color::Black`. Test: `page_color_paints_the_print_layout_sheet`.
2. Text with no colour of its own on a coloured sheet uses `page_ink(rgb).0` as foreground: light
   ink (`#F2F2F2`) on a dark sheet, dark ink (`#202020`) on a light sheet. Tests:
   `page_color_paints_the_print_layout_sheet`, `page_color_light_sheet_uses_dark_ink`, `page_ink_picks_a_side_by_luminance`.
3. Read Mode (continuous, `page_view == false`) does not paint the page colour; with no page
   colour, Print Layout is drawn exactly as before. Test: `page_color_not_painted_outside_print_layout`
   plus the existing page-view/watermark render tests staying green.
4. Control verb `doc.page-color {color}` (`"#RRGGBB"` or `"none"`) sets/removes `w:background`
   and `w:displayBackgroundShape` exactly as the Page Color picker does, survives save, and returns
   `{"pageColor": "#RRGGBB" | null, "changed": bool}`.
5. Control verb `doc.watermark {text, layout?, font?, color?}` or `{remove: true}` writes/removes the
   text watermark in every shown header exactly as the Watermark picker does (header parts + one
   undo step for the new header references), survives save, and returns
   `{"watermark": "<text>" | null, "changed": bool}`.
6. Control verb `doc.page-borders {border, color?}` (`border` = `"none" | "box" | "shadow"`) writes the
   same `w:pgBorders` the Page Borders picker writes (with `w:color` when `color` is given) to every
   section as **one undo step**, survives save, and returns `{"pageBorders": "<border>", "changed": bool}`.
7. All three verbs are classified `MutationKind::Formatting`, so read-only / formatting-locked
   protection denies them before any document, package or history change. On a Markdown document
   each returns an error containing `needs a .docx`.
8. MCP tools `docxy_page_color`, `docxy_watermark`, `docxy_page_borders` forward to those verbs,
   appear in `tool_defs()` after `docxy_compare`, and the JS mirror (`server.mjs`) and the committed
   snapshot (`tools-expected.json`) match: `cargo test -p docxy`, `cargo test -p xlsxy` and
   `node offxy-vscode/mcp/parity.test.mjs` all pass.
9. The ribbon pickers behave exactly as before: every existing `design_*`, `page_color_pick_*`,
   `watermark_pick_*`, `page_borders_pick_*` test in `docxy/src/main.rs` passes unchanged.

## Approach

**Shared App methods.** Today `App::apply_design_pick` (`docxy/src/main.rs`, ~line 4753) does the
three edits inline. Extract each arm's *edit* into an App method that both the picker and the
control verbs call, so automation cannot drift from the ribbon:

- `fn set_page_color(&mut self, rgb: Option<u32>) -> bool`
- `fn set_text_watermark(&mut self, spec: Option<&TextWatermarkSpec>) -> Result<bool, String>`
- `fn set_page_borders(&mut self, pb: Option<&PageBorders>) -> bool`
- free fn `fn box_page_borders(shadow: bool, color: Option<u32>) -> PageBorders`

Each method starts with the existing "commit an open header/footer edit" step
(`if self.hf_edit.is_some() { self.exit_hf_edit(true); }`), moved out of `apply_design_pick`.
`apply_design_pick` keeps only item→value mapping and its status strings.

**Page colour painting.** Generalise `paint_page_on_black(line)` into
`paint_page(line, sheet: Color, ink: Color)` (same algorithm: leading blank text is desktop/black,
the rest is the sheet; spans with no `fg` get `ink`). In `App::draw`'s content paragraph, compute
the sheet colour once per draw: `self.page_view.then(|| self.pkg.page_background()).flatten()`;
when it is `Some`, paint with that colour and `page_ink` regardless of `light_page`; otherwise keep
today's two branches byte-for-byte. Copy the suite's `page_ink` (oracle below) into `main.rs`; docxy
cannot depend on the suite crate.

Rejected alternative: caching the page colour in an App field. It saves a decode of
`document.xml` per frame, but adds a staleness hazard (reload, open, undo of other edits) for a
value `doc_notice` already re-reads every draw; not worth it here.

**Automation.** Three verbs in `control.rs`, classified Formatting in `mutation_kind_for_verb`,
calling the App methods above. Three MCP tools mapping to them, mirrored in `server.mjs`, snapshot
regenerated with the existing dump test. Docs (`docs/agent-control.md`,
`docs/docx-mutation-inventory.md`, `docxy/src/skill.rs` verb list) updated.

Verb arguments (all colours `"#RRGGBB"`, case-insensitive hex, error text
`bad color '<v>' (want "#RRGGBB")`, the same wording `doc.format` uses):

| verb | args | result |
|---|---|---|
| `doc.page-color` | `color`: `"#RRGGBB"` or `"none"` (required) | `{pageColor: "#RRGGBB"\|null, changed}` |
| `doc.watermark` | `text` (non-empty after trim) with optional `layout` (`"diagonal"` default \| `"horizontal"`), `font` (default `"Calibri"`), `color` (default `#C0C0C0`); **or** `remove: true` | `{watermark: "<text>"\|null, changed}` |
| `doc.page-borders` | `border`: `"none"\|"box"\|"shadow"` (required); optional `color` | `{pageBorders: "<border>", changed}` |

Errors (exact strings, `<verb>` is the full verb name e.g. `doc.page-color`):
- Markdown document: `"<verb> needs a .docx (not Markdown)"`.
- `doc.page-color` without `color` string: `"doc.page-color needs a 'color' (\"#RRGGBB\" or \"none\")"`.
- `doc.watermark` with both `text` and `remove: true`: `"doc.watermark takes 'text' or 'remove', not both"`;
  with neither (or blank text): `"doc.watermark needs a 'text' (or 'remove': true)"`; bad layout:
  `"bad layout '<v>' (want \"diagonal\" or \"horizontal\")"`; header impossible: the Err from
  `set_text_watermark`, `"could not add the watermark: the document cannot take a header"`.
- `doc.page-borders` bad/missing border: `"doc.page-borders needs a 'border' (\"none\", \"box\" or \"shadow\")"`.

The watermark spec for the verb is `TextWatermarkSpec::preset(text, diagonal)` with `font` and
`color` overridden when given (size stays Auto, semitransparent stays true).

## Files

- `docxy/src/main.rs` — App methods, `apply_design_pick` refactor, `paint_page`, `page_ink`, draw, tests.
- `docxy/src/control.rs` — 3 verbs, classification, tests.
- `docxy/src/mcp.rs` — 3 tools, `verb_for`, test tables.
- `docxy/src/skill.rs` — verb list line.
- `offxy-vscode/mcp/server.mjs` — 3 tool defs + `DOCXY_VERBS` entries.
- `offxy-vscode/mcp/tools-expected.json` — regenerated.
- `offxy-vscode/mcp/parity.test.mjs` — count 78 → 81 and its message.
- `docs/agent-control.md`, `docs/docx-mutation-inventory.md` — verb rows.

## Tests

See **Tests first** for names. Commands:

```
cargo fmt --all
cargo clippy -p docxy --all-targets -- -D warnings
cargo test -p docxy
cargo test -p xlsxy mcp
node offxy-vscode/mcp/parity.test.mjs
```

## Out of scope

- **plan-dialogs** (Custom Watermark, More Colors / Fill Effects, the full Page Border tab): a
  feature-sized TUI dialog set; filed as its own follow-up issue. The new verbs already let
  automation set an arbitrary colour, watermark text/font/colour and border colour.
- Gradients / fill effects in painting: a gradient page paints its base `color` only.
- `docxwasm/src/bridge.rs` (VS Code / Offxy editors): the new verbs are terminal-only, like the
  issue; a VS Code tab answers them with `unknown verb`. Not filed separately unless review asks.
- Painting the page colour in Read Mode, the PDF export, or the HTML page.
- Adding `pageColor`/`pageBorders` keys to `doc.path` (the verbs return the state they set).

## Open questions

None for the human. (Formatting class for all three follows the existing inventory row
"Document layout … page colour, watermark, page borders → Formatting".)

## Exact edits

1. `docxy/src/main.rs`, `impl App`, **next to `apply_design_pick`**: add `set_page_color`,
   `set_text_watermark`, `set_page_borders` (bodies = the corresponding arm of today's
   `apply_design_pick` minus its `self.status = ...` lines, plus the hf-edit commit at the top).
   - `set_page_color(rgb)`: build `PageBackground { color, gradient: None }` from `rgb`,
     `self.pkg.set_page_background(bg.as_ref())`, set `self.modified = true` when changed, return changed.
   - `set_text_watermark(spec)`: today's watermark arm verbatim (sections, `replace_sections`,
     `page_parts_sect.clear()`, `sync_page_parts()`, `refresh_watermark_state()`, `modified`), then
     `Err("could not add the watermark: the document cannot take a header".into())` when
     `spec.is_some()` and `shown_text_watermarks` is empty, else `Ok(changed)`.
   - `set_page_borders(pb)`: today's borders arm's `edit_sections` call + `after_edit()` when
     changed; return changed.
2. `docxy/src/main.rs`, free fn `box_page_borders(shadow, color)` returning the four-sided
   `PageBorders` the picker builds today (`single`, sz 4, space 24, `color`, `shadow`, frame false,
   AllPages, offset Page, z_order_back false).
3. `docxy/src/main.rs`, `apply_design_pick`: **only** replace each arm's edit code with a call to
   the new method; keep the status strings identical (`Page color: <item>` / `Page color: No Color`,
   `Watermark: <item>` / `Watermark removed` / `Could not add the watermark: the document cannot take a header`
   (capital C, as today), `Page borders: <item>`), the final `self.dirty = true`, and the
   `unreachable!` arm. The PageBorders arm sets the status **after** the call (after_edit clears it).
4. `docxy/src/main.rs`: rename `paint_page_on_black(line)` to `paint_page(line, sheet: Color, ink: Color)`
   — replace `Color::White` with `sheet` and the default fg `Color::Black` with `ink`; desktop stays
   `Color::Black`. Update its one caller to `paint_page(l, Color::White, Color::Black)`.
5. `docxy/src/main.rs`: add `fn page_ink(sheet: u32) -> (u32, u32)` — a copy of
   `suite/docxy/src/design_tab.rs:311-320` — and `fn rgb_color(rgb: u32) -> Color` (`Color::Rgb`).
6. `docxy/src/main.rs`, `App::draw`, **only** the `let mut para = if self.light_page { ... }` block
   (~line 6412): add a leading branch for `Some(rgb)` page colour in page view as in Approach;
   the two existing branches stay unchanged.
7. `docxy/src/control.rs`: in `mutation_kind_for_verb` add
   `"doc.page-color" | "doc.watermark" | "doc.page-borders"` to the Formatting arm; in `dispatch` add
   the three routes; add handlers `page_color`, `watermark`, `page_borders` and a local
   `fn parse_rgb(s: &str) -> Result<u32, String>` (`#` + exactly 6 hex digits). Each handler calls
   `ctlcore::signal_activity()` only when `changed` (as `replace_all` does). Do **not** add them to
   the unconditional `matches!(verb, ...)` activity list in `dispatch`.
8. `docxy/src/mcp.rs`: `verb_for` entries `docxy_page_color`, `docxy_watermark`,
   `docxy_page_borders`; three `tool(...)` defs appended **after** `docxy_compare` with
   `target()`; required arrays `["color"]`, `[]`, `["border"]`. Extend the tests'
   `VERB_TABLE`, `expected_tail`, `required_of` assertions and the
   `mutating_mcp_tools_forward_to_control_authorized_verbs` `expected` list (Formatting).
9. `offxy-vscode/mcp/server.mjs`: three `tool(...)` defs after `docxy_compare` with the same names,
   descriptions, props and required arrays as Rust; `DOCXY_VERBS` entries.
10. `offxy-vscode/mcp/tools-expected.json`: regenerate per
    `dump_tool_defs_json_for_mcp_parity_snapshot`'s doc comment (`docxy/src/mcp.rs`): dump docxy
    and xlsxy, concatenate docxy's array then xlsxy's. Do not hand-edit.
11. `offxy-vscode/mcp/parity.test.mjs`: `78` → `81`, message adds "+ #955 page background".
12. `docxy/src/skill.rs`: add the three verbs to the `Verbs:` line and the three tools to the MCP list.
13. `docs/agent-control.md`: rows for the three verbs in the verb table (args, result, undo
    behaviour: page colour and watermark parts are package edits with no undo; watermark header
    references and page borders are one undo step). `docs/docx-mutation-inventory.md`: three rows in
    "Control and MCP routes", class Formatting.

## Must not change

- The picker items, `PAGE_COLORS`, `WATERMARK_PRESETS`, `PAGE_BORDER_ITEMS`, the ribbon, and every
  picker status string.
- `docxcore` (no edits at all; CLAUDE.md keeps it dependency-free and this needs nothing new there).
- `docxwasm/src/bridge.rs`, `suite/`, `ribbon.rs`.
- The existing two `light_page` branches of the draw block, the watermark overlay, image overlay
  and comments panel drawing.
- Existing tests: do not edit any existing test in `main.rs`; in `control.rs`/`mcp.rs` only
  **extend** the listed tables.
- The existing MCP tool order before `docxy_compare`.
- `doc.path`'s output.

## Oracle

- Painting: `suite/docxy/src/design_tab.rs` `page_sheet_color` / `page_ink` (lines 304-320) —
  luminance threshold 0.45, inks `0xF2F2F2`/`0x202020`.
- Edits: `docxy/src/main.rs` `apply_design_pick` (current code) and its tests
  `page_color_pick_sets_background_and_survives_save`, `watermark_pick_adds_header_watermark_and_round_trips`,
  `page_borders_pick_applies_to_every_section_and_undoes` — the verbs must produce the same XML.
- Verb/MCP shape: `docxy/src/control.rs` `set_style` + its tests; `docxy/src/mcp.rs`
  `docxy_set_style` tool def and the parity snapshot procedure in its doc comments;
  `offxy-vscode/mcp/parity.test.mjs`.

## Tests first

Write these first; each must fail (or not compile) on the current code.

`docxy/src/main.rs` tests module (use `app_with`, `TestBackend`, `term.backend().buffer()`):
1. `page_ink_picks_a_side_by_luminance` — `page_ink(0xFFFFFF) == (0x202020, 0x808080)`,
   `page_ink(0x000000) == (0xF2F2F2, 0xB0B0B0)`, `page_ink(0xFF0000).0 == 0xF2F2F2`, `page_ink(0xFFFF00).0 == 0x202020`.
2. `page_color_paints_the_print_layout_sheet` — `app_with(&["body"])`, set Red via
   `app.set_page_color(Some(0xFF0000))`, `page_view = true`, `light_page = false`, draw on a
   100-wide `TestBackend`; the cell holding the `b` of `body` has bg `Color::Rgb(255,0,0)` and fg
   `Color::Rgb(0xF2,0xF2,0xF2)`; column 0 of that row has bg `Color::Black`.
3. `page_color_light_sheet_uses_dark_ink` — Yellow (`0xFFFF00`), `light_page = true`: `b` cell bg
   `Rgb(255,255,0)`, fg `Rgb(0x20,0x20,0x20)`.
4. `page_color_not_painted_outside_print_layout` — Red, `page_view = false`: no buffer cell has bg
   `Rgb(255,0,0)`.
5. `picker_and_page_color_method_write_the_same_document_xml` — side effect: one app picks "Red"
   through the picker, another calls `set_page_color(Some(0xFF0000))`; their `document.xml` and
   `settings.xml` part texts are equal.

`docxy/src/control.rs` tests module:
6. `page_color_verb_sets_removes_and_survives_save` — `dispatch(.., "doc.page-color", {color:"#ff0000"})`
   → `pageColor == "#FF0000"`, `changed == true`; save via `doc.save` to a temp path (set
   `app.path` under `std::env::temp_dir()`, as main.rs's `save_and_reload` does), `load_package`
   the bytes, `page_background().color == 0xFF0000`; then `{color:"none"}` → `pageColor` null,
   `app.pkg.page_background()` is None.
7. `watermark_verb_adds_custom_text_and_removes` — `{text:"Internal", layout:"horizontal", color:"#FF0000"}`:
   `shown_text_watermarks` has one with text `Internal`, rotation ≈ 0, `app.modified`; survives
   save (reload, shown watermark text still `Internal`); `{remove:true}` → none shown, `watermark` null.
8. `page_borders_verb_writes_every_section_in_one_undo_step` — two-section doc (copy the
   construction from main.rs `page_borders_pick_applies_to_every_section_and_undoes`, or
   `app_with` + a sectPr; any doc with 2 sections), `{border:"shadow", color:"#00FF00"}`: every
   section parses with `shadow` and `color == Some(0x00FF00)`; one `doc.undo` removes all
   `<w:pgBorders`; survives save (`w:shadow="1"` in the reloaded `sect_pr()`).
9. `design_verbs_validate_arguments` — each error string listed in Approach, and nothing modified.
10. `design_verbs_refuse_markdown` — on a Markdown app each verb errs with `needs a .docx`.
11. Extend `control_mutation_classification_covers_every_mutating_dispatch_verb` and
    `dispatch_denies_every_mutating_verb_before_document_package_or_history_changes` with the
    three verbs (valid args), so read-only protection leaves package, sectPr and doc unchanged.

`docxy/src/mcp.rs`: extended tables (Exact edit 8) fail until the tools exist.

## Pitfalls

- **Header/footer edit open**: while `hf_edit` is open, `self.editor` is the header's temporary
  editor. Each new method must commit it first, or page borders land in the header editor and a
  header part is written back over the new watermark. (Test `watermark_pick_commits_open_header_edit_first` guards the picker.)
- **after_edit clears `status`**: in `apply_design_pick` the PageBorders status is set after the
  method returns; don't reorder.
- **Undo**: page colour and watermark *parts* are package edits with no undo (as today); only the
  section edits are undo steps. Do not add new undo bookkeeping.
- **Protection** is enforced by `dispatch` via `mutation_kind_for_verb` before handlers run;
  handlers must not mutate before validating their arguments (validate everything, then call the
  App method once).
- **Gold vs Orange**: two palette colours share `0xFFC000`; the verbs report hex, never a name.
- **Parity snapshot**: `tools-expected.json` is compared order- and key-sensitively by the Rust
  test; regenerate via the dump test, never hand-edit. The JS server must match by hand.
- `page_background()` decodes `document.xml`; call it at most once per draw, and only in page view.
- `paint_page` must keep treating the first non-blank cell as the page start: page lines begin with
  the frame (`│`, `┌`, `└`), inter-page gap lines are all blank → all desktop.
- `cargo test -p docxy` runs some tests that write temp files; use unique dir names per test.

## Done means

Run, all green:

```
cargo fmt --all
cargo clippy -p docxy --all-targets -- -D warnings
cargo test -p docxy
cargo test -p xlsxy mcp
node offxy-vscode/mcp/parity.test.mjs
```

Commit on the issue branch (one or more commits), then report
`IMPLEMENTED <sha>` with: the new test names, the pass/fail/ignored counts from each command, and
**every skipped or ignored test by name** (or "none"). Note anything from this plan you did not do.
