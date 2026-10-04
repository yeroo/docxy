# Review scope r1 — yeroo/docxy#955 (Leftovers from #940: terminal Design tab)

Plan: .workbench/plan.md (agreed v1; amendments: `paint_page_on_black` kept as a wrapper of the new `paint_page`; doc edits extended).
Diff range: `origin/main..HEAD` (commit d6d1764).

## What the change is for
1. Terminal docxy Print Layout tints each page sheet with the document's page colour (`w:background`), ink chosen by luminance (oracle: suite/docxy/src/design_tab.rs `page_ink`, lines 304-320).
2. New control verbs `doc.page-color`, `doc.watermark`, `doc.page-borders` and MCP tools `docxy_page_color`, `docxy_watermark`, `docxy_page_borders`, sharing new App methods (`set_page_color`, `set_text_watermark`, `set_page_borders`, `box_page_borders`) with the Design ribbon pickers. MCP mirrored in offxy-vscode/mcp/server.mjs with the regenerated snapshot tools-expected.json.

## Acceptance criteria
See plan.md "Acceptance criteria" 1-9.

## Look hardest at
- docxy/src/main.rs `apply_design_pick` refactor: behaviour must be identical to before (status strings, hf_edit commit ordering, after_edit clearing status, modified flag, undo steps).
- Draw path: no page colour => output byte-identical to before; page colour => sheet/desktop painting; interaction with selection highlight, watermark overlay, images, comments panel.
- control.rs verbs: argument validation happens before any mutation; protection (Formatting) gating; error paths that leave the document half-modified (e.g. `set_text_watermark` returning Err after a change); Json result shapes; `signal_activity` only on change.
- MCP: Rust tool_defs vs server.mjs vs tools-expected.json parity; descriptions accurate.
- Docs (docs/agent-control.md, docs/docx-mutation-inventory.md, docxy/src/skill.rs, control.rs module doc table) accurate and complete.

## Out of scope
- The Design tab dialogs (deferred as a follow-up issue).
- docxwasm/bridge.rs (VS Code tabs): the verbs are terminal-only.
- Read Mode / PDF / HTML painting of the page colour; gradients beyond the base colour.

## How to test
`cargo test -p docxy`, `cargo test -p xlsxy mcp`, `node offxy-vscode/mcp/parity.test.mjs`, `cargo clippy -p docxy --all-targets -- -D warnings`.
Do NOT run interactive or GUI tests (no launching docxy in a terminal).
