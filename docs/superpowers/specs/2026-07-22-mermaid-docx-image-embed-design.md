# Mermaid → .docx image embed — design

**Goal:** When a `.docx`/`.md` is saved from the docxy editor, embed each
` ```mermaid ` diagram as a **crisp image rendered by real mermaid.js** (matching
the webview preview exactly) instead of the hand-rolled DrawingML shapes — so the
Word file looks identical to the pixel-perfect preview. The Mermaid **source is
preserved**, so md↔docx round-trip still recovers the fence and reopening
re-renders. The pure CLI (no mermaid.js) keeps the improved editable shapes.

**Basis:** conversational request (2026-07-22). The webview already renders real
mermaid.js (merged live-render slice); our own layout engine can't match Mermaid
and an offline SVG harness can't faithfully verify Word output — so for Word
fidelity, embed the real Mermaid render as a picture. Chosen: do the embedding in
the Rust/wasm engine (it owns OOXML/image parts); the webview supplies the
rendered image bytes. No extension-host round-trip — the wasm engine and
`Session::save` run **in the webview**, exactly where mermaid.js already rendered.

## Architecture — webview renders, engine embeds

```
webview (has wasm engine + mermaid.js):
  for each mermaid box in the doc:
    svg  = mermaid.render(source)          // already done for the preview
    png  = rasterize(svg) via <canvas>     // new: high-DPI PNG
  build imageMap: source -> { png: bytes, svg: bytes, wEmu, hEmu }
  on SAVE: call session.save_with_mermaid_images(imageMap)   // new wasm export
        │
        ▼
docxwasm / docxcore Session.save:
  serialize the document as today, BUT for each run holding a mermaid SmartArt
  drawing (identified by descr "mermaid:<source>"):
    replace the <w:drawing> shape-group with a PICTURE drawing:
      <pic:pic> -> <a:blip r:embed="rIdPng"> + <a:extLst><a:ext><asvg:svgBlip
      r:embed="rIdSvg"/></a:ext></a:extLst>   (PNG fallback + crisp SVG)
      docPr@descr keeps "mermaid:<source>"     (round-trip carrier preserved)
    add media parts word/media/mermaidN.png + .svg, and the rels
    size the picture wp:extent = (wEmu, hEmu) from the render
```

## Components

### 1. Webview (`offxy-vscode/media/webview.js`)
- Reuse the Phase-1 mermaid render (SVG per source, cached). Add **PNG
  rasterization**: draw the SVG into an `<img>`→`<canvas>` at a high device scale
  (e.g. 2–3×) and `canvas.toBlob('image/png')` → bytes. Compute the picture EMU
  size from the SVG's natural width/height (px → EMU at 96dpi).
- Build an **image map** keyed by the exact mermaid source string:
  `{ [source]: { png: Uint8Array, svg: Uint8Array, wEmu, hEmu } }`.
- On save, pass the map to a new wasm call `save_with_mermaid_images(mapJson,
  pngBlobs...)` (exact ABI in the plan; images cross the wasm boundary as byte
  buffers, the map JSON carries the source→index + dims). If a diagram failed to
  render (no image), the engine leaves its SmartArt shapes untouched (fallback).
- Async: rendering is async and already cached; ensure the save awaits all
  pending renders (or renders on-demand at save for any uncached source).

### 2. Bridge (`docxwasm/src/bridge.rs`)
- New export `save_with_mermaid_images(...)` alongside `save()`: accepts the
  PNG (and SVG) bytes + a JSON descriptor (source string, byte lengths, wEmu,
  hEmu per diagram) and calls a docxcore save that performs the substitution.
  `save()` (no images) stays as-is.

### 3. Engine (`docxcore` — save/serialize + a new image-embed module)
- At serialize time, walk the document for `Inline::SmartArt { raw, .. }` where
  `mermaid::source_of(raw)` is `Some(src)` and `src` matches a supplied image.
  Replace that run's `<w:drawing>` with a **picture drawing** (`pic:pic`) that:
  - references a PNG blip (`r:embed`) and, via the `a:svgBlip` extension, an SVG
    blip (crisp in Word 2016+, PNG fallback elsewhere);
  - sets `wp:extent`/`a:ext` to the render's EMU dims;
  - keeps `docPr@descr = "mermaid:<escaped source>"` so `source_of` still
    recovers it → md↔docx round-trip and re-render on reopen both work.
- Register the media parts (`word/media/mermaidN.png` / `.svg`) and the
  document rels (`rIdPng`/`rIdSvg`), and add PNG/SVG to `[Content_Types].xml`.
  Reuse existing image/relationship/media plumbing (`docxcore` already loads
  and models images).
- A diagram with no supplied image (CLI path, or a failed render) serializes its
  SmartArt shapes exactly as today — the improved editable-shape fallback.

### 4. CLI / markdownToDocx (unchanged)
No mermaid.js → no image map → `to_drawing` shapes as today (the parked
cycle-break/multiline/sizing improvements are the fallback).

## Round-trip & re-render
- `descr` still carries `mermaid:<source>` on the picture, so `docx_to_md`
  recovers the ` ```mermaid ` fence byte-faithfully (existing `source_of` path,
  now reading the picture's `descr` instead of the shape-group's).
- Reopening the `.docx` in the docxy editor: the engine loads the picture as a
  mermaid `SmartArt`/image with its source; the webview re-renders it live with
  mermaid.js (the picture is the on-disk artifact; the live view is mermaid.js).

## Error handling
- No image for a source (render failed / CLI) → keep the SmartArt shapes; never
  a blank.
- Malformed/oversized PNG → skip that one diagram's substitution (shapes remain);
  save never fails because of an image.
- SVG-blip is additive; a Word version that ignores it shows the PNG.

## Testing
- **docxcore:** a unit test that, given a doc with a mermaid SmartArt run + a
  fake PNG/SVG for its source, the saved package contains a `pic:pic` with a PNG
  blip + `asvg:svgBlip`, the media parts, the rels, `[Content_Types]` entries,
  and the `descr` still carries `mermaid:`; `source_of` on the new drawing still
  returns the source (round-trip); a diagram with NO supplied image keeps its
  shape group (fallback). Re-open the saved bytes and assert the mermaid source
  survives.
- **docxwasm:** `save_with_mermaid_images` round-trips (save → load → source
  intact); `save()` unchanged (byte-identical to today).
- **webview:** a headless/jsdom test that the PNG rasterization + image-map build
  produces a PNG blob and correct EMU dims for a sample SVG; the save call passes
  the map.
- **Manual (deferred to maintainer):** open a real `.docx` with an embedded
  mermaid image in Microsoft Word and confirm it renders crisply and matches the
  preview; confirm round-trip to `.md`.
- Gates: fmt/clippy, `cargo test` (docxcore/docxwasm), wasm build, extension
  typecheck/build/package, `test:mcp-parity` 56/56, `test:md-roundtrip`.

## Out of scope
- Making the embedded picture editable-as-a-Mermaid in Word (it's an image; the
  editable representation is the live webview + the preserved source).
- CLI-side rendering (the pure `docxy.exe` has no mermaid.js; it keeps shapes).
- xlsx/pptx; any agent/ctl/MCP change; version bump.
