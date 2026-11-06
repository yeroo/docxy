# E-mail HTML export

Read these notes before changing `docxcore::export_email`, the writer that turns a `Document` into a mail body.
This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository
root. Add this area's new notes here as `##` sections. Not to be confused with the `html-export` cargo feature
(the suite's Editable HTML, `htmlbundle`), which wraps the docx with the wasm engine and is not a mail body.

## Document to e-mail-safe HTML (#1948)

- `docxcore::export_email::to_email_html(&Document, Option<&Package>) -> EmailHtml`, pure and std only.
  `EmailHtml.html` is a whole UTF-8 document (`<!DOCTYPE html>`, `<meta charset="utf-8">`).
  `EmailHtml.media` lists each picture once, in order of first use. Each entry is an `EmailMedia`:
  `content_id` (as in the `cid:` URL, without `cid:` or `<>`), `bytes`, `file_name` (the part's own
  name, `image1.png`) and `content_type` (the package's, else by extension, else
  `application/octet-stream`). A caller attaches each one inline with that Content-ID. Wiring
  the writer into Mail is a later issue.
- **Inline styles only.** Mail clients strip `<style>` and ignore classes, so the page has neither
  (`no_style_element_or_class_attribute`). Every look is a `style` attribute or a presentational
  attribute: `<ol type start>`, `<img width height>`, `<td colspan rowspan>`, `<table cellspacing cellpadding>`.
- **Context.** The styles and lists come from `ExportContext::for_package` (a new Markdown package's
  without a package). The document is `final_view`'d (tracked changes accepted), and `ctx.markers` is
  computed **on the final view**: its keys are tree paths, and accepting a deleted paragraph mark shifts
  them.
- **Runs.** Each run is a `<span>` of its *effective* look, resolved as the RTF writer resolves it
  (`export_context::effective_run`, with the user's underline and strike; the two writers also share
  `text_box_paragraphs` and `marker_format`). A highlight is written as
  `background-color:#RGB;mso-highlight:name`: Outlook and the HTML import read the second, every other
  client the first. Because of the `background-color`, a round trip also adds run shading (`w:shd`).
  A `code` run with no font of its own is `'Courier New',monospace`, over the default font. Inside
  `<hN>` a run that is not bold says `font-weight:normal`. Hidden text is decided by
  `StyleSheet::effective_vanish`, so a style's `w:vanish` hides too: hidden text must never leave in a
  sent message. That includes what the model keeps as a raw run: a picture (neither shown nor
  attached), a text box, a note reference, SmartArt, a chart or a run-based equation. Their run's own
  `w:rPr` is read by `load::raw_run_props`; an OMML equation has no run of its own: only its
  paragraph's style can hide it. The text
  of SmartArt, a chart or an equation takes that run's look too.
- **Spaces.** A space that would collapse is `&nbsp;`: one at a line's start or end, or after another
  space. Imported back, these come as U+00A0.
- **Tabs.** A tab is Word's tab span, `<span style="mso-tab-count:1;white-space:pre">\t</span>`. The HTML
  import reads any span whose style has `mso-tab-count` as one tab and skips its content. A bare `\t`
  would collapse to a space.
- **Paragraphs.**
  - `<p>`, or `<h1>`..`<h6>` for `heading_level` 1-6. A numbered heading is a heading, not a list item.
  - `margin` is always written with all four sides, 0 when unset, so a client's own `p` margin never
    shows. An unset spacing therefore imports back as `Some(0)`, not `None`.
  - `text-indent` is the first-line or hanging indent.
  - `text-align` follows `effective_align`. A right-to-left paragraph (`dir="rtl"`) that is Left is
    `right`, as the renderer aligns it.
  - Line spacing: `auto` is `line-height:<n/240>`; `atLeast` is `line-height:Npt` (Word's own HTML);
    `exact` adds `mso-line-height-rule:exactly`, without which the import reads `atLeast`.
  - Only the paragraph's own spacing and indent are written: `StyleSheet` resolves no paragraph
    spacing.
  - An empty paragraph is `&nbsp;`, so a client keeps its line.
- **Lists.**
  - Consecutive list paragraphs (`num_id` non-zero, not headings) nest by `ilvl` (0..=8): `<ul>` for a
    bullet level, else `<ol type="1|a|A|i|I">`. A level the numbering does not define takes its kind
    from its marker.
  - A deeper item opens inside the open `<li>`. A jump of several levels opens the lists in between
    directly. A different `num_id` or kind at the same level closes and reopens the list.
  - A plain paragraph or a table ends every open list.
  - Numbering goes on as in Word: `Numbering::step` advances the same counters as `compute_markers`
    (per abstract list, deeper levels restart). An `<ol>` opened for an item whose number is not 1
    says `start`. A list opened between levels has no `start` and shows 1 next: when an item reuses
    it, or any other list, and its number is not the one the browser would show next, the item says
    `<li value="N">`. The HTML import ignores `start`, so `numbered_list_continues_after_a_paragraph`
    tests it on the string.
  - The import maps every bullet list to numId 1 and every numbered list to numId 2.
  - An item's left and first-line indent are the client's list padding: they are not written.
  - Lists that only a paragraph style numbers (`StyleSheet::style_num_pr`) are plain paragraphs, as in
    RTF.
- **Links.** A target whose scheme is `http`, `https`, `ftp` or `mailto` (any case, trimmed) is the
  `href`, from the HTML import's own allow-list (`import::html::LINK_SCHEMES`). Otherwise the in-document anchor (`#name`) is used, otherwise no `<a>` and the text alone:
  `javascript:`, `data:`, `file:` and relative targets are never links. A link that shows nothing (all its
  text hidden, by its runs or a style, or deleted) writes nothing at all, so its URL never leaves in the
  message source (`Writer::hyperlink` takes the `<a>` back when `Writer::shown` did not move). The import turns a `"` in an
  `href` into `%22`.
- **Pictures.**
  - An inline DrawingML picture (`image::inline_embed`) resolves its rid against `word/document.xml`.
    It is written as `<img src="cid:ID" width height alt style="width:Wpx;height:Hpx;border:0">`, with W and H
    being its extent / `EMU_PER_PX`, at least 1.
  - The Content-ID is `image<n>.<FNV-1a 64 of the bytes, hex>@docxy`, `n` being the order of first
    use. It is the same on every export of the same document, and never shared by two different
    pictures; the same picture can still get a different id in another document. The same part used
    twice is one medium.
  - Without a package, a relationship or the part, the picture is its Alt Text (`wp:docPr descr`).
  - Floating pictures are not written. Image formats are not converted: EMF, WMF and TIFF go out as
    they are.
  - Round-trip tests need a picture narrower than the import's body width (the import scales a wider
    box down) and bytes that `image::sniff` recognises (else it uses a placeholder). The resolver gets
    the whole `cid:` src.
- **Tables.** `<table cellspacing=0 cellpadding=0 style="border-collapse:collapse">`, with cells
  `vertical-align:top;padding:0 5.4pt`, `colspan` from `grid_span`, and `rowspan` counted over a
  `VMerge::Restart` and the `Continue` cells below it at the same grid column (`GridMap`).
  Continuation cells are skipped. Borders, widths and shading are not written.
- **Other content.** Fields, equations, SmartArt and charts are written as their text, a text box as
  its paragraphs joined by `<br>`, a note reference as `<sup>N</sup>`. Breaks are `<br>`. Headers,
  footers, note bodies, comments and raw-XML content are not written.
