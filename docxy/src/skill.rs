//! The bundled agent skill (agwinterm-style self-onboarding): a `SKILL.md` that
//! teaches an agent to drive a live docxy via the MCP tools or the control pipe.
//! `docxy install skill` writes it to `~/.claude/skills/docxy/` (and
//! `~/.codex/skills/docxy/` when that root exists) so an LLM self-discovers it.

/// The skill document installed for agents to self-onboard.
pub const SKILL_MD: &str = r#"---
name: docxy
description: Use when reading or editing a Microsoft Word (.docx) or Markdown document that is open in a docxy editor (often a sibling agwinterm pane) — read the live document, edit/insert/replace paragraphs, find text, and save, via the docxy_* MCP tools or docxy's control pipe. Edits land on docxy's own undo stack and repaint the editor live, so this beats editing the file on disk (no clobbering unsaved work, no stale reads).
---

# docxy

docxy is a terminal .docx/Markdown editor with a control surface. You can read and
edit the **live** open document — including unsaved changes — instead of touching
the file on disk. Your edits go through docxy's editor: they are undoable and
repaint the pane instantly.

## When to use
The user has a document open in docxy (often in a split pane next to you) and asks
you to read or change it: "tighten the second paragraph of my open doc", "add a
conclusion", "fix the heading typo".

## Preferred: MCP tools
If the `docxy` MCP server is configured (`claude mcp add docxy -- docxy --mcp`),
use its tools:
- `docxy_list` — which docxy editors are running
- `docxy_status` — path, format, modified flag, block count
- `docxy_outline` — heading tree: block index, level, text
- `docxy_read` `{start?, end?}` — live text per block (+ kind); default whole doc
- `docxy_find` `{query, case_sensitive?}`
- `docxy_replace_range` `{start, end?, text}` — replace paragraphs (`\n` = new paragraph)
- `docxy_insert` `{at, text}` — insert before a block
- `docxy_append` `{text}`
- `docxy_save`

When several docxy editors are open, pass `target` (a substring of the pane id;
`docxy_list` shows them). Typical flow: `docxy_outline` or `docxy_read` to see the
structure, then `docxy_replace_range` / `docxy_insert` / `docxy_append`, then
optionally `docxy_save`.

## Fallback: the control pipe
Without MCP, talk to docxy directly over loopback TCP + newline-delimited JSON.
Find the discovery file in `%APPDATA%\docxy\ctl\` (Windows) or
`$XDG_CONFIG_HOME/docxy/ctl/` (Unix): `<instance>.json` = `{port, token, pid}`,
where instance is `docxy-<AGWINTERM_SESSION_ID>` — the pane id shown by
`agwintermctl tree`. Then one request per line:

```
→ {"token":"…","verb":"doc.read","args":{"start":1,"end":3}}
← {"ok":true,"result":{ … }}
```

Verbs: `doc.path`, `doc.outline`, `doc.read`, `doc.find`, `doc.replace-range`,
`doc.insert`, `doc.append`, `doc.save`, `doc.reload`, `doc.open`.

## Two panes in one agwinterm session
`agwintermctl split on`, then launch `docxy <file>` in the new pane (or press
Ctrl+D and run docxy there). Now you sit beside the document and edit it live.

## Notes
- Addressing is by **top-level block index**; `docxy_read` / `doc.read` reports
  each block's `kind`, so you know which indices are paragraphs (edit verbs need
  paragraph endpoints).
- In `text`, `\n` separates paragraphs.
- Read before you write. A `replace_range` is a delete+insert — the user can undo
  it in docxy (Ctrl+Z), same as any edit.
- While you edit, the docxy pane's status dot flashes active, so the user sees the
  document being worked on.
"#;

/// Install `SKILL.md` for self-discovery (see [`ctlcore::install_skill`]).
pub fn install() -> std::io::Result<String> {
    ctlcore::install_skill("docxy", SKILL_MD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_frontmatter_and_verbs() {
        assert!(SKILL_MD.starts_with("---\nname: docxy\n"));
        assert!(SKILL_MD.contains("description:"));
        for needle in [
            "docxy_replace_range",
            "doc.replace-range",
            "AGWINTERM_SESSION_ID",
            "agwintermctl split on",
        ] {
            assert!(SKILL_MD.contains(needle), "skill missing {needle}");
        }
    }

    #[test]
    fn install_writes_skill_md_under_home() {
        let tmp = std::env::temp_dir().join(format!("docxy_skill_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let msg = ctlcore::install_skill_to(&tmp, "docxy", SKILL_MD).unwrap();
        let claude_skill = tmp
            .join(".claude")
            .join("skills")
            .join("docxy")
            .join("SKILL.md");
        assert!(claude_skill.exists(), "SKILL.md written under ~/.claude");
        assert_eq!(std::fs::read_to_string(&claude_skill).unwrap(), SKILL_MD);
        assert!(msg.contains("docxy"));
        // ~/.codex didn't exist, so it must have been skipped.
        assert!(!tmp.join(".codex").exists());

        // With ~/.codex present, it installs there too.
        std::fs::create_dir_all(tmp.join(".codex")).unwrap();
        ctlcore::install_skill_to(&tmp, "docxy", SKILL_MD).unwrap();
        assert!(
            tmp.join(".codex")
                .join("skills")
                .join("docxy")
                .join("SKILL.md")
                .exists()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
