# Manual bundles (#1022)

Each editor's manual is written in each editor's spec repository and
published here as a bundle that the hosts read through the `helpdocs` crate. This file is the format those
repos publish to. `helpdocs/tests/fixture/` holds a small example of each.

## Layout

`docs/manual/<editor>/`, where `<editor>` is `word`, `excel` or `project`:

- `REVISION`: one line, the spec-repo commit the bundle was published from.
  The suite shows it in About docxy suite and under every Help page (cut to
  7 characters when it is a commit); a blank line counts as no revision.
- `contents.txt`: the contents tree, one page id per line, nested by two
  spaces of indent per level. A line starting `#` is a comment. The first
  entry is the manual's root page. Every page is listed exactly once.
- `<id>.md`: one per page. The id is the file stem, `[a-z0-9-]+`.
- `images/`: the pages' pictures, referenced as `![alt](images/x.png)`.

A page starts with front matter:

```
---
title: Home tab
area: home                        # optional
keys: b, i, dialog:font           # optional
---
```

- `title`: shown in the contents and search. Without one the id is used.
- `area`: the area this page belongs to (see Context help below).
- `keys`: the context ids this page answers, comma-separated.

In `area` and `keys`, a ` #` and everything after it is a comment. The
title is kept as written, so `How to correct a #N/A error` is a title. The
front matter opens on a first line that is `---` and ends at the next such
line (trailing spaces allowed on both). Without a closing line it is
reported and the whole file is read as the body.

## Markdown subset

Blocks:
- ATX headings `#` to `####`.
- Paragraphs.
- Bullet lists (`-` or `*`) and numbered lists (`1.`), nested by indent.
  A numbered list starts at its first item's number (`3.` starts at 3). A
  line that is not a block of its own continues the item above it. A list
  goes on across blank lines while the next item is deeper, or at its indent
  and of its kind. An image, fenced code block or paragraph indented under
  an item belongs to the nearest item above it with a smaller indent (a
  screenshot under a step, then its explanation, even after the step's
  sub-bullets). Its place among the item's sub-lists is kept.
- Fenced code blocks, kept literal. A fence is three or more backticks and
  closes at a line of only backticks, at least as many. An opening line
  whose info string holds a backtick is not a fence.
- GFM pipe tables. A table needs a header row and an alignment row
  (`:---`, `:---:`, `---:`) with one cell per header cell; otherwise the
  lines are a paragraph. Body rows may have fewer or more cells. `\|` is a
  pipe everywhere in a table row, code spans included, and a `|` inside a
  code span or a `<kbd>` chord never splits a cell.
- An image on a line of its own.

Inline (an inline `![alt](src)` is literal text):
- `**bold**`, `*italic*` and `` `code` ``. An emphasis opener must be
  followed by a non-space and a closer preceded by one, so `2 * 3 * 4` is
  plain text.
- Links `[text](target)`.
- Key chords `<kbd>Ctrl+Shift+L</kbd>`. The chord is split on `+`, so a
  plus key is written `<kbd>Ctrl+Plus</kbd>`, or `+` where a key is due
  (`<kbd>Ctrl++</kbd>`, `<kbd>Ctrl++Shift</kbd>`, `<kbd>+A</kbd>`). An empty `<kbd></kbd>` is literal text.

A link target is one of:
- another page (`home-font`, or `home-font#effects` for one of its headings);
- a heading of the same page (`#effects`);
- an external `http:` or `https:` address, which a host marks as leaving the
  manual.

Anything else is reported as a broken link.

Anything outside the subset is shown as literal text, never an error.

Headings get GitHub-style slugs: the text is lowercased, spaces become `-`,
and punctuation other than `-` and `_` is dropped. A repeated slug gets `-1`,
`-2` and so on.

## Where the suite reads a manual (#1855)

The suite's Help tab reads an editor's manual from
`<config root>/docxy/manual/<editor>/` (an installed bundle, the same layout as
here), else the one built into the binary from `docs/manual/<editor>/`. The
config root is `DOCXY_CONFIG_DIR` when set, else the OS's config directory. A
bundle with no page counts as none: Help and F1 then say the documentation is
coming. A harness instance reads only its sandbox's config root, where a
script's `manual <editor> <dir>` line copies a bundle; the built-in manuals
never reach a `.uit` case. Images are drawn when they are PNG, JPEG, GIF or
BMP; any other file is a box with its alt text.

## Where the terminal editors read a manual (#1856)

Terminal `docxy`, `xlsxy` and `yppxy` show the same manuals (word, excel,
project) as text when F1 is pressed. They read the installed bundle in the
suite's place, `<config root>/docxy/manual/<editor>/`, so one install serves
both. The config root is `DOCXY_CONFIG_DIR`, else the OS's config directory
(`%APPDATA%`, `~/Library/Application Support`, `$XDG_CONFIG_HOME` or
`~/.config`). Without one they use the bundle built in from
`docs/manual/<editor>/` when the binary was built. Images are shown as
`[image: alt]`.

## Context help

F1 resolves a context id to a page:
- A ribbon command id is used bare: the ribbon's own command id, which
  differs per editor (the document's Bold is `b`, the sheet's `bold`). The
  ids each ribbon draws are the ones `every_ribbon_command_has_a_help_target`
  walks (`suite/docxy/src/help_tab/tests.rs`). F1 asks for it while the
  pointer rests on the command (its ScreenTip need not be up), with the
  ribbon tab shown as the area.
- A dialog id is `dialog:<id>` (`dialog:font`).
- An area is the ribbon tab's name, slugified by the heading rule
  (`Home` → `home`, `Page Layout` → `page-layout`,
  `Header & Footer` → `header--footer`).

Resolution:
1. A page whose `keys` list the id. If two pages claim one key, the first in
   contents order keeps it, and the second is reported.
2. Otherwise, the area's page: the page whose id is the area, else the first
   page whose `area` is it.
3. Otherwise, the root (the first entry in the contents).

It never fails on a manual with pages.

`every_ribbon_command_has_a_help_target` (in the suite) lists how every
ribbon command resolves. It is a report until a manual marks an area strict
with a line before its first contents entry:

```
# strict: home, insert
```

From then on, a command of that area without a page of its own fails the
test, and so does a strict area no command belongs to (a misspelt or
renamed tab). The directive is case-sensitive and counts only before the first
entry; later on it is a plain comment.

## Problems

A bundle always loads. These are reported (`Manual::problems`):
- no `contents.txt`;
- a listed page with no file;
- a page file the contents do not list (it still loads);
- an id listed twice;
- a bad page id;
- a key claimed by two pages;
- a broken link;
- a missing image;
- a `.md` file in a subdirectory (not loaded);
- a page whose front matter is never closed.
