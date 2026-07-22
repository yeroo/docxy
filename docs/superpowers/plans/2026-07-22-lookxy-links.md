# lookxy Links (Phase 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development
> or superpowers:executing-plans. Steps use `- [ ]`. Executed inline.

**Goal:** Blue inline links (no footnotes), Ctrl+↑/↓ link navigation with a
bottom URL status, and a safe open-in-browser warning dialog (raw/parsed);
click-a-link too.

**Architecture:** htmlrender drops the footnote scheme + hard-wraps long words;
`App` gains a derived `body_links` list, `focused_link`, and a `link_prompt`
modal. Reuse `StyledSpan::link`, the existing `rundll32` opener, and a ported
`safe_url`.

## Global Constraints

- MSRV 1.88. Full workspace green, `clippy --all-targets -D warnings` clean,
  `fmt` clean. Build via `bash "$LCARGO" …`. Each task committed.
- Spec: `docs/superpowers/specs/2026-07-22-lookxy-links-design.md`.

---

## Task 1: htmlrender — inline links, no footnotes + hard-wrap (mailcore)

**Files:** `mailcore/src/htmlrender.rs` (+ its tests).

- [ ] **Step 1 — failing test:** `<a href="u">hi</a>` → a single span `text:"hi",
  link:Some("u")`, and the whole render contains no `[1]` and no appendix line.
  Plus: a 200-char unbroken URL as anchor text wraps to multiple lines each
  ≤ the width.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Drop `footnotes`/`n`: `<a>` pushes just the href
  string; `</a>` pops it (no `[n]` marker). Remove the footnote-appendix block.
  In the word-flush/wrap path, break a word longer than `width` into
  width-sized chunks (carry its style/link onto each chunk).
- [ ] **Step 4 — update the existing `[n]`/footnote assertions to the new shape;
  run, expect PASS.**
- [ ] **Step 5 — commit:** `mailcore: inline links without footnotes; hard-wrap long words`.

---

## Task 2: Blue links + body_links extraction (lookxy)

**Files:** `lookxy/src/ui/reading.rs` (`to_ratatui_span` color; extractor),
`lookxy/src/app.rs` (`body_links`, `focused_link`, populate in `reload_body`).

**Interfaces:** `pub struct BodyLink { line: usize, col: u16, width: u16, url: String }`;
`App::body_links: Vec<BodyLink>`; `App::focused_link: Option<usize>`.

- [ ] **Step 1 — failing tests:** `to_ratatui_span` on a linked span → `fg ==
  Blue`. And after `reload_body` on a 2-link HTML body, `app.body_links.len() ==
  2` with the right urls and `focused_link == None`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Change the link color to `Color::Blue`. Add
  `reading::collect_links(&[StyledLine]) -> Vec<BodyLink>` folding each line's
  spans into contiguous linked runs (tracking the running column incl. indent).
  In `reload_body`, after building the owned lines, store
  `self.body_links = collect_links(&lines)` and `self.focused_link = None`.
  (Factor the owned-lines build so both draw and `reload_body` use it, or
  recompute the lines in `reload_body` at the reader's width — keep the
  coordinate model consistent with `to_ratatui_line`'s indent.)
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: blue links + body_links extraction`.

---

## Task 3: Ctrl+↑/↓ navigation + scroll-into-view + URL status (lookxy)

**Files:** `lookxy/src/app.rs` (`focus_link(delta)`), `lookxy/src/ui/mod.rs`
(Ctrl+↑/↓ routing), `lookxy/src/ui/reading.rs` (highlight focused + URL strip).

- [ ] **Step 1 — failing test:** with 2 links loaded and `focus == Reading`,
  `focus_link(1)` sets `focused_link == Some(0)` and scrolls so
  `body_links[0].line` is within `[reading_scroll, reading_scroll+viewport)`;
  a second `focus_link(1)` → `Some(1)`; `focus_link(1)` again clamps at
  `Some(1)`; `focus_link(-1)` → `Some(0)`.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** `focus_link(delta)`: no-op on empty list; from
  `None`, delta>0 → 0, delta<0 → last; else clamp `idx+delta` into range; set
  `reading_scroll` to bring `line` into view (if above, scroll up to it; if
  below `scroll+viewport`, scroll so it's the last visible row). Route
  `KeyCode::Up`/`Down` with `KeyModifiers::CONTROL` when `focus == Reading` to
  `focus_link(∓1)`, ahead of the plain scroll arms. In `reading::draw`, draw the
  focused link's run reversed, and — when `focused_link` is set — render the URL
  in a bottom strip of the reading area, wrapped to ≤3 rows.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: Ctrl-arrow link navigation + focused-URL status`.

---

## Task 4: Open-with-warning dialog + safe_url + URL parse (lookxy)

**Files:** `lookxy/src/app.rs` (`link_prompt`, open/toggle/confirm, `safe_url`,
`parse_url_parts`, opener reuse), `lookxy/src/ui/linkprompt.rs` (draw + keys),
`lookxy/src/ui/mod.rs` (route + draw).

**Interfaces:** `pub struct LinkPrompt { url: String, parsed: bool }`;
`App::link_prompt: Option<LinkPrompt>`; `App::open_focused_link()`.

- [ ] **Step 1 — failing tests:** `safe_url("https://x")` true, `safe_url("javascript:x")`
  false; `parse_url_parts("https://acme.com/a/b?x=1")` → scheme https, host
  acme.com, path /a/b, query x=1. `open_focused_link` with a focused http link
  opens `link_prompt` (url set, parsed false); the dialog's toggle flips
  `parsed`; confirming a safe URL calls the opener + clears the prompt; a
  `javascript:` URL sets a "blocked" status and does not open.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** `safe_url` (port docxy). `parse_url_parts(&str) ->
  Vec<(&'static str, String)>` (scheme/host/path/query, present parts only).
  `open_focused_link`: if a link is focused, set `link_prompt = Some{ url, parsed:false }`.
  Dialog handler: `Esc` → clear; `p`/`Tab` → flip `parsed`; `Enter` → if
  `safe_url` call the opener (the existing `open_path`-style rundll32 fn) + clear,
  else set `error_notice = "blocked non-web link: …"` + clear. `linkprompt::draw`
  renders the centered overlay (raw or parsed view + footer). Route in
  `handle_key` at the top (like backstage) and add to `is_capturing_text`; draw
  full-frame-overlay after the panes; Enter in the Reading pane (no Ctrl) with a
  focused link calls `open_focused_link`.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: open-link warning dialog (raw/parsed) + safe_url`.

---

## Task 5: Click a link to open it (lookxy)

**Files:** `lookxy/src/app.rs` (`on_left_click` reading branch).

- [ ] **Step 1 — failing test:** with `reading_rect`/`body_links` set and a link
  at a known (line,col), a left-click on that cell opens `link_prompt` for its
  URL; a click off any link just focuses Reading.
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** In the `reading_rect` branch of `on_left_click`,
  map the click to `(line, col)` via `reading_rect` + `reading_scroll`, find a
  `BodyLink` whose `line` matches and `col..col+width` contains the click; if
  found, open its `link_prompt`; else focus Reading as today.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: click a link to open the warning dialog`.

---

## Self-Review

- **Coverage:** htmlrender inline+wrap (T1); blue + extraction (T2); nav +
  status (T3); dialog + safe open (T4); click (T5). Every spec section maps.
- **Type consistency:** `BodyLink`/`body_links`/`focused_link`/`LinkPrompt`
  names shared across tasks; `safe_url`/`parse_url_parts` in T4.
- **Coordinate risk:** `body_links` line/col must match `to_ratatui_line`'s
  indent + wrap. Derive the list from the same laid-out lines the reader draws;
  T2 notes this.
- After each task: `test --workspace`, `clippy --all-targets -D warnings`, `fmt`.
  Final whole-branch review (direct), then push to PR #21.
