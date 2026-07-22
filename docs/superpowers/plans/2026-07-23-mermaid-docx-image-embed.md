# Mermaid → .docx Image Embed Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** On save, replace each ` ```mermaid ` SmartArt drawing in the `.docx` with a **picture** rendered by real mermaid.js (PNG + crisp SVG blip), keeping the Mermaid source in `descr` so md↔docx round-trip and live re-render still work. The pure CLI (no mermaid.js) keeps the editable-shape fallback.

**Architecture:** The wasm engine + `Session::save` run in the webview, where mermaid.js already rendered. The webview rasterizes a PNG from the Mermaid SVG and passes an image map to a new `save_with_mermaid_images` export; `docxcore` adds media parts + rels + Content-Types and rewrites each matching mermaid drawing to a `pic:pic`. Engine-first so the OOXML validity/round-trip risk is proven in Task 1.

**Tech Stack:** Rust std-only `docxcore` (OOXML), `docxwasm` (new export), `offxy-vscode/media/webview.js` (canvas PNG rasterization + save call).

## Global Constraints

- `docxcore` std-only, ZERO external dependencies.
- `save()` (no images) stays **byte-identical** to today; the new path is additive.
- The rewritten drawing MUST keep `docPr@descr="mermaid:<escaped source>"` so `mermaid::source_of` and the loader (`load.rs` drawing→SmartArt) still recover the source (md↔docx round-trip + reopen re-render). A diagram with NO supplied image is left unchanged (fallback).
- No agent/ctl/MCP change (`test:mcp-parity` 56/56). No extension version bump.
- Windows: `export PATH="$HOME/.cargo/bin:$PATH"` before cargo; never pipe an exit-code command through `tail`. Subagents must NOT drive the live desktop; headless/unit only, live Word e2e deferred to the maintainer.

## Current seams (context)

- `docxwasm::bridge::Session::save(&mut self) -> Vec<u8>` (bridge.rs:1115) does `self.pkg.document = self.editor.doc.clone(); save_package(&self.pkg)`.
- `Inline::SmartArt { raw: String, text: Vec<String> }` — `raw` is the `<w:drawing>` XML, emitted verbatim on save. `mermaid::source_of(raw) -> Option<String>` reads `descr`; a generated mermaid drawing has `descr="mermaid:<escaped src>"`.
- `docxcore::package::Package` has the add-part pattern (a method ~package.rs:174 adds a part + `[Content_Types].xml` override + a `document.xml.rels` relationship for headers; comments at :273 do the same). `next_rid(rels_xml)` mints a fresh `rId`. `part`/`set_part` read/write parts.
- The document root already declares the `r:` relationships namespace (existing images/hyperlinks use `r:embed`/`r:id`).

---

### Task 1: Engine — embed a Word-valid picture (PNG + svgBlip), replacing a mermaid SmartArt

**Files:** Create `docxcore/src/mermaid_embed.rs`; Modify `docxcore/src/lib.rs` (add `pub mod mermaid_embed;`), `docxcore/src/package.rs` (add a media-part helper if none exists).

**Interfaces:**
- `pub struct MermaidImage { pub source: String, pub png: Vec<u8>, pub svg: Vec<u8>, pub w_emu: i64, pub h_emu: i64 }`
- `pub fn embed_images(pkg: &mut Package, doc: &mut Document, images: &[MermaidImage])` — for each `Inline::SmartArt` in `doc` whose `mermaid::source_of(raw)` matches an image's `source`, add media parts + rels + content-types to `pkg` and rewrite the inline's `raw` to a picture drawing. Non-matching / no-image SmartArt untouched.
- `package.rs`: `pub(crate) fn add_media_part(&mut self, bytes: &[u8], ext: &str) -> String` → adds `word/media/imageN.<ext>`, ensures a `[Content_Types].xml` Default for `<ext>`, adds a `document.xml.rels` Relationship of type `…/image`, returns the new `rId`. (Mirror the existing header/comment add-part code.)

- [ ] **Step 1: Write failing tests** (`mermaid_embed.rs` `#[cfg(test)]`).

```rust
#[test]
fn embeds_picture_with_png_and_svg_blip() {
    // Build a doc with one mermaid SmartArt via the markdown path.
    let mut doc = crate::markdown::from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
    let mut pkg = crate::package::new_markdown_package(&doc); // or the test constructor the crate uses
    let img = MermaidImage { source: "flowchart TD\nA-->B".into(),
        png: vec![0x89,0x50,0x4E,0x47], svg: b"<svg/>".to_vec(), w_emu: 3_000_000, h_emu: 1_500_000 };
    embed_images(&mut pkg, &mut doc, &[img]);
    // The inline's drawing is now a picture referencing a PNG + SVG blip.
    let raw = /* find the single SmartArt inline's raw in doc */;
    assert!(raw.contains("<pic:pic"), "{raw}");
    assert!(raw.contains("a:blip") && raw.contains("r:embed="));
    assert!(raw.contains("svgBlip"), "svg blip missing: {raw}");
    assert!(raw.contains("cx=\"3000000\"") && raw.contains("cy=\"1500000\""));
    // Source preserved for round-trip.
    assert_eq!(crate::mermaid::source_of(raw).as_deref(), Some("flowchart TD\nA-->B"));
    // Media parts + rels + content-types present.
    assert!(pkg.part("word/media/image1.png").is_some() || pkg.part("word/media/mermaid1.png").is_some());
    let rels = String::from_utf8_lossy(pkg.part("word/_rels/document.xml.rels").unwrap()).into_owned();
    assert!(rels.contains("/image") && rels.contains(".png") && rels.contains(".svg"));
    let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
    assert!(ct.contains("png") && ct.contains("svg"));
}

#[test]
fn no_image_leaves_shapes_untouched() {
    let mut doc = crate::markdown::from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
    let mut pkg = crate::package::new_markdown_package(&doc);
    let before = /* the SmartArt raw */.to_string();
    embed_images(&mut pkg, &mut doc, &[]); // no images supplied
    assert_eq!(/* the SmartArt raw */, before, "unmatched diagram must be unchanged");
}

#[test]
fn embedded_picture_round_trips_to_markdown() {
    let mut doc = crate::markdown::from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
    let mut pkg = crate::package::new_markdown_package(&doc);
    let img = MermaidImage { source: "flowchart TD\nA-->B".into(), png: vec![1,2,3], svg: b"<svg/>".to_vec(), w_emu: 3_000_000, h_emu: 1_500_000 };
    embed_images(&mut pkg, &mut doc, &[img]);
    pkg.document = doc.clone();
    let bytes = crate::package::save_package(&pkg);
    // Reload and confirm the mermaid source survives (source_of on the loaded drawing).
    let reloaded = crate::package::load_package(&bytes).unwrap();
    let md = crate::markdown::to_markdown(&reloaded.document);
    assert!(md.contains("```mermaid") && md.contains("flowchart TD"), "{md}");
}
```
(Adapt the doc/pkg constructors + the "find the SmartArt raw" helper to the crate's real APIs — match what `markdown.rs`/`package.rs` tests already use.)

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid_embed`

- [ ] **Step 3: Implement `add_media_part`** in `package.rs` mirroring the existing header/comment add-part (new `word/media/imageN.<ext>` part; a `<Default Extension="<ext>" ContentType="image/<ext or svg+xml>"/>` in `[Content_Types].xml` if absent; a `<Relationship Id="rIdN" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/imageN.<ext>"/>` appended to `word/_rels/document.xml.rels` via `next_rid`+`replacen`); return the `rId`. SVG content-type = `image/svg+xml`.

- [ ] **Step 4: Implement `embed_images`.** For each `Inline::SmartArt { raw, .. }` in `doc` (walk paragraphs/inlines) where `mermaid::source_of(raw)` equals an image `source`: `let rid_png = pkg.add_media_part(&img.png, "png"); let rid_svg = pkg.add_media_part(&img.svg, "svg");` then set the inline's `raw` = `picture_drawing_xml(rid_png, rid_svg, img.w_emu, img.h_emu, &img.source)`. Keep the `Inline::SmartArt` variant (so `source_of` + loader still treat it as mermaid) — only its `raw` changes. Emit this exact drawing shape (namespaces inline; `r:` is declared on the doc root):

```
<w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"
  xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
  <wp:extent cx="{w}" cy="{h}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>
  <wp:docPr id="1" name="Mermaid Diagram" descr="{descr}"/>
  <wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>
  <a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
   <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">
    <pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
     <pic:nvPicPr><pic:cNvPr id="1" name="mermaid.png" descr="{descr}"/><pic:cNvPicPr/></pic:nvPicPr>
     <pic:blipFill><a:blip r:embed="{rid_png}">
       <a:extLst><a:ext uri="{{28A0092B-C50C-407E-A947-70E740481C1C}}">
         <asvg:svgBlip xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main" r:embed="{rid_svg}"/>
       </a:ext></a:extLst></a:blip>
      <a:stretch><a:fillRect/></a:stretch></pic:blipFill>
     <pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{w}" cy="{h}"/></a:xfrm>
      <a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>
    </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>
```
where `descr = format!("mermaid:{}", mermaid::escape_source(&source))` — REUSE the exact same `descr` encoding `mermaid::to_drawing` uses so `source_of` round-trips byte-faithfully. Add `pub mod mermaid_embed;` to `lib.rs`.

- [ ] **Step 5: Run — expect PASS + no regressions.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (new tests + existing markdown/package/mermaid tests green).

- [ ] **Step 6: fmt/clippy; commit.**

```bash
git commit -am "docxcore: embed mermaid diagrams as pic:pic (PNG + svgBlip), source preserved"
```

---

### Task 2: Bridge — `save_with_mermaid_images` wasm export

**Files:** Modify `docxwasm/src/bridge.rs`; Test in `bridge.rs`.

**Interfaces:** `pub fn save_with_mermaid_images(&mut self, images_json: &str, png_blob: &[u8], svg_blob: &[u8]) -> Vec<u8>` — `images_json` describes each diagram `{source, pngOff, pngLen, svgOff, svgLen, wEmu, hEmu}` slicing the two concatenated blobs (or per-image byte arrays if the ABI is cleaner that way — decide in impl, keep it simple and documented). Builds `Vec<MermaidImage>`, calls `mermaid_embed::embed_images(&mut self.pkg, &mut self.editor.doc.clone()…)` then `save_package`.

- [ ] **Step 1: Write the failing test.**

```rust
#[test]
fn save_with_images_embeds_and_round_trips() {
    let doc = docxcore::markdown::from_markdown("```mermaid\nflowchart TD\nA-->B\n```\n");
    let mut s = /* Session constructor the neighboring save/view_json tests use */;
    let json = r#"[{"source":"flowchart TD\nA-->B","pngOff":0,"pngLen":4,"svgOff":0,"svgLen":6,"wEmu":3000000,"hEmu":1500000}]"#;
    let bytes = s.save_with_mermaid_images(json, &[0x89,0x50,0x4E,0x47], b"<svg/>");
    // Saved package embeds a picture; source survives a reload.
    let reloaded = docxcore::package::load_package(&bytes).unwrap();
    assert!(docxcore::markdown::to_markdown(&reloaded.document).contains("flowchart TD"));
    // A plain save() is unchanged.
    let plain = s.save();
    assert!(!String::from_utf8_lossy(&plain).contains("svgBlip")); // no image path
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxwasm save_with_images`

- [ ] **Step 3: Implement.** Parse `images_json` (hand-rolled or the crate's json helper), slice the blobs into per-image `png`/`svg` `Vec<u8>`, build `Vec<MermaidImage>`, set `self.pkg.document = self.editor.doc.clone()`, call `mermaid_embed::embed_images(&mut self.pkg, &mut self.pkg.document.clone()…)` — careful with borrow: embed operates on the doc that will be serialized. Simplest: clone doc, embed into (pkg, &mut doc), set `pkg.document = doc`, `save_package(&pkg)`. Clear dirty. `save()` stays untouched.

- [ ] **Step 4: Run — expect PASS + no regressions + gates.**
```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p docxcore && cargo test -p docxwasm && cargo fmt && cargo clippy -p docxcore -p docxwasm --all-targets -- -D warnings
cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:mcp-parity
```
(mcp 56/56; md-roundtrip green.)

- [ ] **Step 5: Commit.** `git commit -am "docxwasm: save_with_mermaid_images export (embed webview-rendered images)"`

---

### Task 3: Webview — rasterize PNG, build image map, embed on save

**Files:** Modify `offxy-vscode/media/webview.js`; Test `offxy-vscode/media/mermaid-embed.test.mjs`.

**Interfaces:** Consumes the Phase-1 mermaid SVG render (cached by source) + the Task-2 `save_with_mermaid_images`. Produces PNGs + the image map, and routes save through the new export when the doc has mermaid diagrams with rendered images.

- [ ] **Step 1: Read the current save path.** Find where the webview serializes/saves (calls `session.save()` / posts bytes to the host). The new path calls `save_with_mermaid_images` instead when `lastView.mermaid` has entries with rendered SVGs.

- [ ] **Step 2: PNG rasterization helper + failing test.** Add `async function svgToPng(svgString, wEmu, hEmu) -> Uint8Array`: load the SVG into an `Image` via a `data:image/svg+xml;base64,…` URL, draw onto a `<canvas>` at a high pixel scale (target ≥ ~2× the display px; derive px from EMU/914400*96*scale), `canvas.toBlob('image/png')` → `arrayBuffer` → `Uint8Array`. Test (`mermaid-embed.test.mjs`, jsdom or a canvas stub): given a known SVG, `svgToPng` resolves to a non-empty `Uint8Array` starting with the PNG signature `89 50 4E 47`, and the EMU→px scaling is correct. (If jsdom lacks canvas, stub `canvas.toBlob`/`Image` minimally and assert the map-building + scaling logic; note the rasterization itself is browser-verified/deferred.) Add `"test:mermaid-embed"` to package.json scripts.

- [ ] **Step 3: Image-map build + save wiring.** On save, for each `lastView.mermaid[i]` with a cached mermaid SVG (from Phase-1 render) and a `source`: rasterize the PNG, collect `{source, png, svg, wEmu, hEmu}` (EMU from the SVG natural size). Concatenate the blobs + build the `images_json` descriptor matching Task-2's ABI, and call `session.save_with_mermaid_images(json, pngBlob, svgBlob)` instead of `session.save()`. If there are no mermaid diagrams (or none rendered), call the plain `save()` (unchanged). Ensure any not-yet-rendered visible diagram is rendered before save (await).

- [ ] **Step 4: Run test + gates.**
```bash
cd offxy-vscode && node media/mermaid-embed.test.mjs && export PATH="$HOME/.cargo/bin:$PATH" && npm run typecheck && npm run build && npm run test:mermaid-embed && npm run test:md-roundtrip && npm run test:mermaid-live && npm run test:mcp-parity && npm run package
```
All green; mcp 56/56.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "offxy webview: rasterize mermaid PNG + embed images into .docx on save"`

---

## Notes for the executor

- Task 1 is the de-risk task (Word-valid `pic:pic` + `svgBlip` + media/rels/content-types + source round-trip). Do it first and thoroughly.
- Keep `save()` byte-identical — the image path is a separate export.
- The rewritten drawing stays an `Inline::SmartArt` variant so `source_of` and the loader keep recognizing it as mermaid; only its `raw` XML changes from shapes to a picture. Verify a reload → `source_of` → markdown fence.
- Live Word visual confirmation is the maintainer's; subagents validate via unit tests + `load_package` round-trip only.
- After Task 3, a save from the docxy editor writes a `.docx` whose mermaid diagrams are crisp real-Mermaid images matching the preview, still round-tripping to `.md`.
