# Word import fixtures (#633)

One document that Microsoft Word (16.0.17932) saved four ways, in one run:

| File | Saved as |
|---|---|
| `source.docx` | Word Document, **the oracle** |
| `source.rtf` | Rich Text Format (`\ansicpg1252`, CRLF) |
| `source.htm` | Web Page, Filtered (`charset=windows-1252`, CRLF) |
| `source.pdf` | PDF, exported right after the `.docx` |

The document has:

- a Heading 1 title and Heading 2 sections;
- bold, italic and underlined runs;
- accented Latin, Cyrillic and a euro sign;
- a tab and a manual line break;
- a bulleted and a numbered list;
- a 2×3 table.

The author fields say `docxy`.

## How they are used

`docxcore/tests/word_import.rs` reads the `.docx` with `docxcore::package::load_package` and compares the other three importers against it.

- **RTF:** paragraph texts are equal.
- **Web Page:** paragraph texts are equal with white space collapsed, because a Web Page writes a tab as spaces.
- **RTF and Web Page:** the same bold, italic and underlined runs, heading levels and list membership.
- **PDF:** the same words in reading order, each paragraph starting a line (or a table column on one), the lists and the headings.
- **Damaged `.docx`:** half of `source.docx` recovers a prefix of the oracle's paragraphs.
- **No panics:** every 1/64 prefix of every file imports without panicking.

The rule is that **the `.docx` Word saved is the truth**. When an importer disagrees with it, the importer is wrong, not the fixture.

## Regenerating

`corpus/tools/gen_word_import.ps1` makes all four from one Word session. It replaces the signed-in account's name with `docxy`, padded to the same length.

**Do not regenerate without Word, and only with a person at the machine.**

- The generator drives a real Word through COM and needs a human-attended Word.
- Word may stop a save on a prompt that only a person can answer (`-Visible` shows it).
- On a machine where docxy's own `wordcomshim` is registered for `Word.Application`, that ProgID gets the shim, not Word, and the shim ignores the save format. The script refuses when `Word.Application` resolves to anything but `WINWORD.EXE`. Unregister the shim for the run: under `HKCU\Software\Classes`, remove `Word.Application`, `Word.Application.16` and `CLSID\{000209FF-0000-0000-C000-000000000046}`.
- The script refuses while any Word is running, because it ends by quitting the Word it drove.
- Tests never need Word: the files are committed as Word wrote them.
- `.gitattributes` marks this folder `-text`, so the CRLF in the `.rtf` and `.htm` is kept.
