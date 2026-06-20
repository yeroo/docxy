# docxy comparison corpus

A classified copy of the WordprocessingML "tricky files" test corpus, used to
eyeball docxy's rendering against Word.

## Layout

- `files/` — 248 `.docx` files copied from the OpenXML SDK test assets,
  preserving the original folder structure (one folder per feature: `comment`,
  `table`, `hyperlink`, `chart`, `equation`, `track change`, `watermark`, …).
- `classification.json` — the manifest: every file with its **category** (top
  folder), **tags** (features and known bug/edge cases it exercises), folder,
  and size. Plus per-tag counts and descriptions, and per-category counts.
- `tools/classify.py` — regenerates `files/` + `classification.json` by scanning
  each docx's parts and body XML. Run from the repo root:
  `python corpus/tools/classify.py`.

## Tags

Feature tags (`comments`, `tracked-changes`, `footnotes`, `tables`,
`merged-cells`, `fields`, `toc`, `lists`, `images`, `wmf-emf`, `vml`, `textbox`,
`watermark`, `ole`, `chart`, `smartart`, `math`, `sdt`, `smarttag`, `symbols`,
`multi-column`, `page-borders`, `landscape`, `rtl`, `shading`, `drawing`,
`headers-footers`, `title-page`, `even-odd`, `section-breaks`, `numbering-part`,
`custom-xml`, `protected`, `write-protected`, `encrypted`, `empty`) are detected
by scanning the package. Bug/edge tags (`normalize-edge`, `bug-missing-id`,
`bug-conflicting-id`, `bug-cannot-normalize`, `partial`) come from the corpus's
own folder/file naming.

## Origin & license

These files come from the OpenXML SDK test assets
(`DocumentFormat.OpenXml.Tests.Assets`, dotnet/Open-XML-SDK), which is
distributed under the MIT License. They are included here unmodified for local
testing only.
