# Word 97-2003 (.doc) corpus

Made by Microsoft Word 16.0.17932 (Office LTSC 2024) with
`corpus/tools/gen_doc_corpus.ps1`, one fixture per recipe in that script. Each
`<stem>.doc` was typed into Word and saved as a Word 97-2003 Document. Each
`<stem>.docx` is Word's own reading of that `.doc`, opened again and saved as a
Word Document: it is the oracle `docxcore/tests/legacy_doc.rs` compares against.

| Stem | Covers |
|---|---|
| `plain` | ASCII paragraphs (compressed 8-bit pieces) |
| `cp1252` | cp1252 characters outside ASCII (curly quotes, dashes, euro, ™, ©) |
| `unicode` | Cyrillic, CJK, Greek and an emoji (UTF-16 pieces) |
| `formatting` | bold, italic, underline, strike, size, font, colour runs |
| `align-headings` | Heading 1-3 and left/center/right/justify paragraphs |
| `table` | a 3x3 table between two paragraphs |
| `fields-breaks` | PAGE and DATE fields, a line break, a tab, a page break |

Word 16 no longer fast-saves, so there is no fragmented piece-table fixture.

The author fields read `docxy`: Word stamps the signed-in Office account's name
into every file, and the script overwrites it with `docxy` padded to the same
length after Word quits, so no offset in the binary files changes. Do not edit
these files by hand; regenerate them with the script.
