IMPLEMENT plan v1 (.workbench/plan.md), with these two amendments agreed from your critique:

1. Exact edit 4: do NOT rename. Add the new `paint_page(line, sheet, ink)` and turn
   `paint_page_on_black(line)` into a one-line wrapper `paint_page(line, Color::White, Color::Black)`.
   The production light_page branch keeps calling `paint_page_on_black` (so the wrapper is not dead
   code and clippy stays clean), and the two existing tests at main.rs:8230/:8246 stay byte-identical.
   This keeps "do not edit any existing test in main.rs".

2. Exact edit 13 also covers: docs/agent-control.md:352-356 (the Formatting-verbs sentence gains the
   three), docs/docx-mutation-inventory.md:83-92 (the "all other control verbs" sentence and the MCP
   tool list), and a note on each new verb row that it refuses Markdown outright (unlike
   doc.format/doc.set-style).

Your confirmations are right: pub(crate) where control.rs needs it, `args.get("remove").and_then(Json::as_bool)`,
page_ink's second element unused by painting (keep the tuple as the oracle has it; `#[allow(dead_code)]` is not
needed since the function is used).

Done means as in the plan; reply IMPLEMENTED <sha> with test names, counts per command, and every skipped/ignored test by name.
