//! The bundled agent skill (agwinterm-style self-onboarding): a `SKILL.md` that
//! teaches an agent to drive a live lookxy via the MCP tools or the control pipe.
//! `lookxy install skill` writes it to `~/.claude/skills/lookxy/` (and
//! `~/.codex/skills/lookxy/` when that root exists) so an LLM self-discovers it.

/// The skill document installed for agents to self-onboard.
pub const SKILL_MD: &str = r#"---
name: lookxy
description: Use when reading or triaging corporate Exchange/Outlook mail that is open in a lookxy editor (often a sibling agwinterm pane) — read the live mailbox (folders, messages, bodies, search) and triage (mark read/unread, flag, move, delete, save attachments), via the lookxy_* MCP tools or lookxy's control pipe. Actions go through lookxy's own sync engine (optimistic + outbox to Exchange) and repaint the pane live.
---

# lookxy

lookxy is a terminal Exchange/Outlook mail client with a control surface. You can
read and triage the **live** mailbox — the local, offline-first store, including
optimistically-applied changes — instead of a separate mail session of your own.
Your triage goes through lookxy's own sync engine: an optimistic local update
queued to Exchange via the outbox, and it repaints the pane instantly.

## When to use
The user has mail open in lookxy (often in a split pane next to you) and asks you
to read or triage it: "what's unread in my inbox", "flag the message from Alice",
"move the Databricks thread to Archive", "search my mail for the VDI invite",
"read the top message".

## Preferred: MCP tools
If the `lookxy` MCP server is configured (`claude mcp add lookxy -- lookxy --mcp`),
use its tools:
- `lookxy_list` — which lookxy mail clients are running
- `lookxy_status` — account, sync state, folder/unread counts, pending outbox ops, selection
- `lookxy_folders` — folder tree: id, name, unread/total counts
- `lookxy_messages` `{folder?, limit?, offset?}` — list messages, newest first
- `lookxy_read` `{id}` — full metadata + rendered plain-text body
- `lookxy_search` `{query, limit?}` — local full-text search
- `lookxy_mark` `{id, read}` — mark read/unread
- `lookxy_flag` `{id, flagged}` — flag/unflag
- `lookxy_move` `{id, dest}` — move to another folder
- `lookxy_delete` `{id}` — delete
- `lookxy_attachments` `{id}` — list a message's attachments
- `lookxy_save_attachment` `{id, attachment, dest?}` — save an attachment to disk
- `lookxy_select` `{folder?, id?}` — move the pane's selection
- `lookxy_refresh` — trigger a background sync

When several lookxy editors are open, pass `target` (a substring of the pane id;
`lookxy_list` shows them). Typical flow: `lookxy_folders` or `lookxy_messages` to
see what's there, `lookxy_read` a message, then `lookxy_mark` / `lookxy_flag` /
`lookxy_move` / `lookxy_delete` to triage it.

## Fallback: the control pipe
Without MCP, talk to lookxy directly over loopback TCP + newline-delimited JSON.
Find the discovery file in `%APPDATA%\lookxy\ctl\` (Windows) or
`$XDG_CONFIG_HOME/lookxy/ctl/` (Unix): `<instance>.json` = `{port, token, pid}`,
where instance is `lookxy-<AGWINTERM_SESSION_ID>` — the pane id shown by
`agwintermctl tree`. Then one request per line:

```
→ {"token":"…","verb":"mail.list","args":{"folder":"…"}}
← {"ok":true,"result":{ … }}
```

Verbs: `mail.status`, `mail.folders`, `mail.list`, `mail.read`, `mail.search`,
`mail.mark`, `mail.flag`, `mail.move`, `mail.delete`, `mail.attachments`,
`mail.save-attachment`, `mail.select`, `mail.refresh`.

## Two panes in one agwinterm session
`agwintermctl split on`, then launch `lookxy` in the new pane. Now you sit beside
the mailbox and triage it live.

## Notes
- Addressing is by a message's **id** (from `lookxy_messages` / `lookxy_read`).
- Triage is optimistic and queued to Exchange via the outbox: a delete or move
  repaints the pane immediately and syncs in the background.
- Read before you act.
- While you act, the lookxy pane's status dot flashes active, so the user sees
  the mailbox being worked on.
"#;

/// Install `SKILL.md` for self-discovery (see [`ctlcore::install_skill`]).
pub fn install() -> std::io::Result<String> {
    ctlcore::install_skill("lookxy", SKILL_MD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_frontmatter_and_verbs() {
        assert!(SKILL_MD.starts_with("---\nname: lookxy\n"));
        assert!(SKILL_MD.contains("description:"));
        for needle in [
            "lookxy_mark",
            "mail.move",
            "AGWINTERM_SESSION_ID",
            "agwintermctl split on",
        ] {
            assert!(SKILL_MD.contains(needle), "skill missing {needle}");
        }
    }

    #[test]
    fn install_writes_skill_md_under_home() {
        let tmp = std::env::temp_dir().join(format!("lookxy_skill_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let msg = ctlcore::install_skill_to(&tmp, "lookxy", SKILL_MD).unwrap();
        let claude_skill = tmp
            .join(".claude")
            .join("skills")
            .join("lookxy")
            .join("SKILL.md");
        assert!(claude_skill.exists(), "SKILL.md written under ~/.claude");
        assert_eq!(std::fs::read_to_string(&claude_skill).unwrap(), SKILL_MD);
        assert!(msg.contains("lookxy"));
        // ~/.codex didn't exist, so it must have been skipped.
        assert!(!tmp.join(".codex").exists());

        // With ~/.codex present, it installs there too.
        std::fs::create_dir_all(tmp.join(".codex")).unwrap();
        ctlcore::install_skill_to(&tmp, "lookxy", SKILL_MD).unwrap();
        assert!(
            tmp.join(".codex")
                .join("skills")
                .join("lookxy")
                .join("SKILL.md")
                .exists()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
