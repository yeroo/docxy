# Manual bundles (#1022)

Each editor's manual is written in its private spec repo (`docxy-word-spec`,
`docxy-excel-spec`, `docxy-project-spec`) and published here as a bundle that
the hosts read through the `helpdocs` crate. This file is the format those
repos publish to. `helpdocs/tests/fixture/` holds a small example of each.

## Layout

`docs/manual/<editor>/`, where `<editor>` is `word`, `excel` or `project`:

- `REVISION`: one line, the spec-repo commit the bundle was published from.
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
keys: bold, italic, dialog:font   # optional
---
```

- `title`: shown in the contents and search. Without one the id is used.
- `area`: the area this page belongs to (see Context help below).
- `keys`: the context ids this page answers, comma-separated.

A ` #` and everything after it in a value is a comment.

## Markdown subset

Blocks:
- ATX headings `#` to `####`.
- Paragraphs.
- Bullet lists (`-` or `*`) and numbered lists (`1.`), nested by indent.
- Fenced code blocks, kept literal.
- GFM pipe tables. A table needs a header row and an alignment row
  (`:---`, `:---:`, `---:`).
- An image on a line of its own.

Inline:
- `**bold**`, `*italic*` and `` `code` ``.
- Links `[text](target)`.
- Key chords `<kbd>Ctrl+Shift+L</kbd>`. The chord is split on `+`, so a
  plus key is written `<kbd>Ctrl+Plus</kbd>`.

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

## Context help

F1 resolves a context id to a page:
- A ribbon command id is used bare (`bold`).
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
test. The directive is case-sensitive and counts only before the first
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
- a missing image.
