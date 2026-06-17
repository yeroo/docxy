# Docxy

**A fast terminal (TUI) viewer and editor for Microsoft Word `.docx` documents.**

Docxy opens real `.docx` files in your terminal — text, tables, lists, styles, and
images — lets you edit and save them losslessly, and can export to PDF. It's built
on a small, dependency-free OOXML engine (`docxcore`) with a thin
[ratatui](https://ratatui.rs) UI on top.

> Docxy deliberately doesn't reproduce Word's pixel-perfect layout — it renders a
> faithful, readable view of the document in a character grid.

## Features

- **View & edit** paragraphs, runs (bold/italic/underline/strike/color), and
  **tables** (including merged cells) — navigate and type directly into cells.
- **Lossless save** — everything Docxy doesn't model (bookmarks, fields, content
  controls, section properties) is preserved byte-for-faithful on save.
- **Styles** resolved from `styles.xml`; **lists** numbered from `numbering.xml`.
- **Images**: raster (PNG/JPEG/GIF/BMP/TIFF) rendered as real pixels via
  kitty/iTerm2/**Sixel** graphics, and legacy **WMF/EMF vector** images rasterized
  through the OS (Windows). Floating (frame-anchored) images are projected to their
  real page positions.
- **Find & replace**, full **clipboard** (copies to the OS clipboard too),
  **selection + formatting**, word navigation, and **show-invisibles**.
- **Vim mode** (`--vim`): motions, operators, visual mode, `/` search, `:w`/`:q`.
- Safe **clickable links** — only `http(s)`, shown for confirmation, opened without
  a shell.
- **PDF export**, headless: `docxy in.docx --pdf out.pdf`.

## Install

```sh
cargo install docxy
```

## Usage

```sh
docxy <file.docx>               # open in the editor
docxy <file.docx> --vim         # open with vim keybindings
docxy <file.docx> --pdf <out>   # export to PDF and exit
```

### Keys

| Keys | Action |
|------|--------|
| type · Enter · Backspace · Delete | edit text |
| arrows · Home/End · PgUp/PgDn | move (Ctrl-←/→ by word) |
| Shift + move | select (Esc clears) |
| Ctrl-B / Ctrl-I / Ctrl-U | bold / italic / underline (over selection) |
| Ctrl-L / Ctrl-E / Ctrl-R | align left / center / right |
| Ctrl-A · Ctrl-C · Ctrl-X · Ctrl-V | select all · copy · cut · paste |
| Ctrl-F | find / replace (Tab toggles replace, Ctrl-A replaces all) |
| Ctrl-S · Ctrl-Z · Ctrl-Y | save · undo · redo |
| Ctrl-Q / Esc | quit |
| F2 · F3 · F4 | page view · show marks · table borders |
| mouse | click to move · click a link to open · wheel/drag to scroll/select |

## Image support

Real image pixels need a graphics-capable terminal:

- **WezTerm** (kitty + Sixel + iTerm2) — best.
- **Windows Terminal ≥ 1.22** (Sixel).
- Most other terminals fall back to a labeled placeholder box.

WMF/EMF vector images are rasterized via the OS GDI on Windows; on other platforms
they show as boxes.

## Building from source

```sh
git clone https://github.com/yeroo/docxy
cd docxy
cargo build --release
cargo test
```

The workspace has two crates:

- **`docxcore`** — pure, `std`-only OOXML I/O (ZIP/DEFLATE/XML, the document model,
  rendering, and the from-scratch PDF writer). No third-party dependencies.
- **`docxy`** — the terminal UI (ratatui), clipboard (arboard), and image rendering
  (ratatui-image).

## License

MIT © yeroo
