# lookxy — Link Rendering, Navigation & Safe Open (Phase 3)

**Date:** 2026-07-22
**Status:** design (Phase 3 of the UI/UX pass)

## Goal

Make links first-class in the reading pane: render them **blue inline** (no
footnote clutter), let the reader **jump between them** with Ctrl+↑/↓ (showing
the focused link's full URL in a bottom status area), and **open them safely**
in the browser via a warning dialog that shows the URL raw or as a parsed
breakdown. Also make a **mouse click** on a link open that dialog.

## Global constraints

- Rust 2024, MSRV 1.88; `mailcore` (htmlrender) + `lookxy` (TUI).
- Full workspace green, `clippy --all-targets -D warnings` clean, `fmt` clean.
- TDD, task-by-task, each committed. No new dependencies.
- Reuse existing infra: `StyledSpan::link` already carries each span's href; the
  `rundll32` opener in `app.rs` already opens URLs safely (no `cmd /c start`
  `&`-mangling); port docxy's `safe_url` (`docxy/src/main.rs`).

## Components

### 1. htmlrender — inline links, no footnotes (mailcore)

- **Remove** the `[n]` marker appended on `</a>` and the footnote appendix at the
  end of `render_html`. Anchor text keeps `link: Some(href)`; nothing else is
  added. The `footnotes`/`n` bookkeeping goes away (each `<a href>` just pushes
  its href onto the link stack; `</a>` pops it).
- **Hard-wrap** a single word (e.g. a long bare URL used as anchor text) that
  exceeds the wrap width: break it across lines at the width boundary instead of
  overflowing. (Today the wrapper only breaks between words.)
- Existing htmlrender tests that assert `[n]`/footnote output are updated to the
  no-footnote shape; add a test that a long unbroken URL hard-wraps.

### 2. reading pane — blue links, navigation, URL status (lookxy)

- **Blue:** `to_ratatui_span` colors a linked span `Color::Blue` (was `Cyan`),
  keeping underline.
- **Link list:** when a body loads (`reload_body`), collect an ordered
  `Vec<BodyLink { line: usize, col: u16, width: u16, url: String }>` from the
  laid-out `StyledLine`s — one entry per contiguous linked run — onto `App`.
  Reset `focused_link: Option<usize>` to `None`. (The reader already builds the
  owned `Vec<StyledLine>`; the link list is derived from the same layout so the
  `line`/`col` coordinates match what's drawn.)
- **Navigate:** in the Reading pane, **Ctrl+Down** focuses the next link,
  **Ctrl+Up** the previous (clamped, no wrap). Focusing a link scrolls it into
  view (`reading_scroll` so `line` is within the viewport) and sets
  `focused_link`.
- **Highlight:** the focused link's span draws reversed (on top of blue) so it's
  obvious which one is active.
- **URL status:** while a link is focused, a bottom strip shows the focused
  URL, wrapping across as many rows as needed (bounded, e.g. ≤3 rows then
  ellipsis). Drawn within/над the reading pane's bottom edge.

### 3. Open with warning (lookxy)

- **State:** `App::link_prompt: Option<LinkPrompt { url: String, parsed: bool }>`.
- **Open triggers:** **Enter** while a link is focused in the Reading pane, or a
  **mouse left-click** on a linked cell, sets `link_prompt` with `parsed: false`.
- **Dialog:** a centered overlay titled "Open link?" showing either:
  - raw: the full URL, wrapped; or
  - parsed (`parsed = true`): indented components —
    `protocol: https` / `host: acme.com` / `path: /a/b/file.ext` /
    `query: "k=v&..."` (each present part on its own line).
  - A footer: "Enter open · p toggle view · Esc cancel".
- **Keys (dialog owns them):** `Esc` cancels (`link_prompt = None`); `p` (or
  `Tab`) flips `parsed`; `Enter` opens — **only if `safe_url(url)`** (http/https)
  — via the existing opener, then closes; a non-web URL closes with a
  `"blocked non-web link"` status instead of opening.
- **`safe_url`:** ported from docxy — allows only `http`/`https`.
- **URL parsing:** a small hand-rolled splitter (no new dep): scheme before
  `://`, host up to the next `/` or `?`, path up to `?`, query after `?`. Blank
  parts are omitted.

### 4. Mouse click on a link

- In `on_left_click`'s reading branch: map the clicked `(col, line)` against the
  `body_links` list (using the recorded `reading_rect` + `reading_scroll`); a
  hit opens the `link_prompt` for that URL. A miss just focuses the pane as
  today.

## Key/routing precedence

- The `link_prompt` dialog is a modal: routed at the top of `handle_key` (like
  help/backstage) and included in `is_capturing_text` (so `q` doesn't quit
  under it) and in `on_mouse`'s modal guard.
- `Ctrl+Up`/`Ctrl+Down` are handled in the Reading-pane arm of the Mail match,
  guarded to `focus == Reading`, ahead of the plain Up/Down scroll arms.

## Testing

**htmlrender (mailcore)**
- `<a href="u">text</a>` yields a span with `link: Some("u")`, text `"text"`,
  and **no** `[n]`; no footnote appendix line is emitted.
- A long unbroken URL as anchor text hard-wraps across lines.

**reading / app (lookxy)**
- `to_ratatui_span` styles a linked span blue.
- `reload_body` populates `body_links` for a body with two links; `focused_link`
  starts `None`.
- Ctrl+Down focuses link 0 then 1 and scrolls each into view; Ctrl+Up steps back;
  clamped at both ends.
- Enter on a focused link opens `link_prompt` with the right URL; `p` toggles
  `parsed`; Enter with a safe http URL calls the opener and closes; a `javascript:`
  URL is blocked (status set, not opened).
- `safe_url` allows http/https, rejects others.
- URL parsing splits scheme/host/path/query correctly (incl. no-path, no-query).
- A click on a linked cell opens the dialog for that URL.

## Out of scope

- Editing links, link previews/unfurling, following `#anchor` fragments within
  the message, opening non-web schemes (mailto/file) — blocked by design.
