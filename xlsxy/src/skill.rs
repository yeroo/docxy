//! The bundled agent skill (agwinterm-style self-onboarding): a `SKILL.md` that
//! teaches an agent to drive a live xlsxy via the MCP tools or the control pipe.
//! `xlsxy install skill` writes it via [`ctlcore::install_skill`].

/// The skill document installed for agents to self-onboard.
pub const SKILL_MD: &str = r#"---
name: xlsxy
description: Use when reading or editing a Microsoft Excel (.xlsx) workbook that is open in an xlsxy editor (often a sibling agwinterm pane) — read live cell values and formulas, set cells (with recalculation), clear ranges, find, and save, via the xlsxy_* MCP tools or xlsxy's control pipe. Edits land on xlsxy's own undo stack, recalculate dependents, and repaint the editor live, so this beats editing the file on disk (no clobbering unsaved work, no stale reads).
---

# xlsxy

xlsxy is a terminal .xlsx editor with a real calculation engine and a control
surface. You can read and edit the **live** open workbook — including unsaved
changes — instead of touching the file on disk. Your edits go through xlsxy's
own edit path: they are undoable, recalculate dependent formulas, and repaint
the pane instantly.

## When to use
The user has a workbook open in xlsxy (often in a split pane next to you) and
asks you to read or change it: "what does the total in C10 come from?", "fill in
Q3 numbers", "fix the formula in B4".

## Preferred: MCP tools
If the `xlsxy` MCP server is configured (`claude mcp add xlsxy -- xlsxy --mcp`),
use its tools:
- `xlsxy_list` — which xlsxy editors are running
- `xlsxy_status` — path, modified flag, sheet count, active sheet
- `xlsxy_sheets` — every sheet's index, name, and used size
- `xlsxy_read` `{sheet?, range?}` — non-empty cells (value, formula, text) in a
  range (`A1:C10`); default = the whole used range of the active sheet
- `xlsxy_get` `{ref, sheet?}` — one cell
- `xlsxy_set` `{ref, text, sheet?}` — set a cell: leading `=` makes a formula
  (validated, recalculated), otherwise number/bool/text is inferred like typing
- `xlsxy_clear` `{range, sheet?}` — blank a range (styles kept)
- `xlsxy_find` `{query, sheet?}` — search values and formula text
- `xlsxy_recalc`, `xlsxy_save`

`sheet` is an index or a name and defaults to the active sheet. When several
xlsxy editors are open, pass `target` (a substring of the pane id; `xlsxy_list`
shows them). Typical flow: `xlsxy_sheets` / `xlsxy_read` to see the data, then
`xlsxy_set` per cell, then `xlsxy_save`.

## Fallback: the control pipe
Without MCP, talk to xlsxy directly over loopback TCP + newline-delimited JSON.
Find the discovery file in `%APPDATA%\xlsxy\ctl\` (Windows) or
`$XDG_CONFIG_HOME/xlsxy/ctl/` (Unix): `<instance>.json` = `{port, token, pid}`,
where instance is `xlsxy-<AGWINTERM_SESSION_ID>` — the pane id shown by
`agwintermctl tree`. Then one request per line:

```
→ {"token":"…","verb":"cell.set","args":{"ref":"B4","text":"=SUM(B1:B3)"}}
← {"ok":true,"result":{ … }}
```

Verbs: `wb.path`, `sheet.list`, `sheet.read`, `cell.get`, `cell.set`,
`range.clear`, `find`, `wb.recalc`, `wb.save`, `wb.reload`, `wb.open`.

## Two panes in one agwinterm session
`agwintermctl split on`, then launch `xlsxy <file>` in the new pane (or press
Ctrl+D and run xlsxy there). Now you sit beside the workbook and edit it live.

## Notes
- Refs and ranges are A1-style (`B4`, `A1:C10`).
- Read before you write; check `formula` on cells you're about to overwrite.
- Each `xlsxy_set` / `xlsxy_clear` is one undo group — the user can undo it in
  xlsxy (Ctrl+Z), same as any edit.
- While you edit, the xlsxy pane's status dot flashes active, so the user sees
  the workbook being worked on.
"#;

/// Install `SKILL.md` for self-discovery (see [`ctlcore::install_skill`]).
pub fn install() -> std::io::Result<String> {
    ctlcore::install_skill("xlsxy", SKILL_MD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_frontmatter_and_verbs() {
        assert!(SKILL_MD.starts_with("---\nname: xlsxy\n"));
        assert!(SKILL_MD.contains("description:"));
        for needle in [
            "xlsxy_set",
            "cell.set",
            "AGWINTERM_SESSION_ID",
            "agwintermctl split on",
        ] {
            assert!(SKILL_MD.contains(needle), "skill missing {needle}");
        }
    }
}
