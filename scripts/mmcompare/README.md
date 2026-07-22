# mmcompare

Visual-comparison harness: renders a Mermaid source **two ways** and drops
both, plus a side-by-side composite, into an output directory. This is the
acceptance tool for the mermaid-live-render plan's later phases — real
mermaid.js output is the ground truth docxy's own layout engine is judged
against.

- **real** — the actual `mermaid.js` (v10, from this package's own
  `node_modules`) rendering the raw Mermaid source in a headless browser.
- **ours** — docxcore's own layout engine: `cargo run --example dump_geo`
  (`docxcore/examples/dump_geo.rs`) prints `mermaid::geometry_box(src)`'s
  JSON — the same geometry the Word exporter turns into DrawingML shapes —
  and `buildMermaidSvg`, pulled unmodified out of
  `offxy-vscode/media/webview.js` via a Node `vm` sandbox, turns that JSON
  into the same inline SVG the docxy webview overlays live.
- **cmp** — both PNGs side by side in one image, for a quick look.

## Usage

```sh
cd scripts/mmcompare
npm i
node render.mjs path/to/diagram.mmd [outdir]
# or, for a markdown write-up with a fenced ```mermaid block:
node render.mjs path/to/diagram.md [outdir]
```

(`npm run compare -- path/to/diagram.mmd` works too.)

`outdir` defaults to `out/` (gitignored). For an input named `foo`, it
writes:

- `real-foo.png` / `real-foo.html` — real mermaid.js
- `ours-foo.png` / `ours-foo.html` — docxcore geometry -> `buildMermaidSvg`
- `cmp-foo.png` / `cmp-foo.html` — the two side by side
- `foo.extracted.mmd` — the raw Mermaid source handed to `dump_geo`
  (identical to the input for a `.mmd`; the fenced body for a `.md`)

## How it renders

Every screenshot goes through headless Chromium:
`C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe`
(`--headless=new --disable-gpu --user-data-dir=<unique-tmp-dir>
--hide-scrollbars --window-size=W,H --virtual-time-budget=12000
--screenshot=<absolute-path>`), falling back to a Playwright Chromium under
`%USERPROFILE%\AppData\Local\ms-playwright\chromium-*\chrome-win\chrome.exe`
if Edge isn't installed on the machine.

The side-by-side composite needs no image library: it's produced by
embedding the two already-rendered PNGs as `data:` URIs in one more HTML
page and screenshotting that.

## Requirements

- `cargo build --example dump_geo -p docxcore` must succeed (this script
  shells `cargo run` for it — the first invocation pays the build cost).
- `npm i` in this directory (installs `mermaid@10` locally).
- Edge or a Playwright Chromium install, as above.

## Gitignored

`out/` and `node_modules/` are not committed (see the root `.gitignore`).
