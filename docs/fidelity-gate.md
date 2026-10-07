# The round-trip fidelity gate

Saving must never silently destroy a user's document (#1060). The fidelity gate
opens every corpus `.docx`, saves it with no edits, and compares every package
part with the original. A loss nobody has accepted fails the build. The same
gate runs over `.xlsx` (#1064); [xlsx](#xlsx) below covers what differs.

It is a plain cargo test, `docxcore/tests/fidelity.rs`, and the `fidelity gate
(docx, xlsx)` job runs it in CI on every PR.

## What it checks

The gate round-trips each file the way docxy saves an **edited** document:
`load_package`, then `Editor::new`, then `save_package`. That save regenerates
`word/document.xml` from the semantic model, so it is where untouched
paragraphs of an edited document lose what the model does not understand.

In docxy, a user who saves **without** editing gets
`save_package_preserving_document`, which writes the original parts back. The
gate asserts that path is byte-identical part for part. That check has no
baseline: any difference fails outright. "The gate reports a loss in X"
therefore means "X is lost once the user edits anything in the document". It
does not mean "X is lost by open+save" in docxy. The suite also writes the
original `word/document.xml` back while the body still equals the stored part
(`Package::stores_document`, then `save_package_keeping_document`, #1107), so
there too a reported loss means "lost once the user edits". That save is not
itself gated (see below).

Every part of the original is compared with its saved counterpart:

- **XML parts** (`.xml`, `.rels` and `.vml`, plus any part `[Content_Types].xml`
  types as XML) are compared canonically:
  - Elements and attributes are matched by namespace URI and local name, so a
    prefix rename is equal.
  - Attribute order, namespace declarations, the XML declaration, comments and
    whitespace-only text between elements are ignored.
  - Line ends normalize to LF, and in attribute values a literal tab, CR or LF
    is a space (an XML processor's normalizations).
  - References are decoded first (`&#38;` equals `&amp;`), and CDATA is text.
  - "Whitespace" means space, tab, CR and LF; a non-breaking space is text.
  - Element and attribute content and text are compared exactly, and so is a
    whitespace-only text that is an element's whole content (`<w:t> </w:t>`).
- **Every other part** must be byte-equal. So must any XML part that is not UTF-8
  or is malformed. Malformed covers:
  - mismatched or unclosed tags;
  - a duplicate attribute;
  - an ill-formed or illegal character reference;
  - a `<` in an attribute value;
  - content outside the root.
- Missing and extra parts are reported.

Child lists are aligned in two passes. Identical subtrees are matched first,
then the gaps between them are matched by element name. So one dropped
paragraph or property is one finding, not a cascade over its siblings. Order is
significant (schema order), so a moved child is reported as a loss plus an
extra. A lost or extra subtree is reported once, at its root.

Each file runs under `catch_unwind`: a panic becomes a `panic` finding and a load
failure a `load-error` finding, and the run goes on.

### Report

Each failure is one line:

```
NEW   ext:table/Foo.docx | word/document.xml | lost-element | /w:document/w:body/w:tbl/w:tr[2]/w:trPr/w:cantSplit
STALE ext:bar.docx | word/document.xml | lost-attr | /w:document/w:body/w:p/@w:rsidR  (fixed: remove it from baseline.txt)
```

The kinds are:

| Kind | Meaning |
|---|---|
| `lost-element` | An element, or the text of an element, is gone. |
| `lost-attr` | An attribute is gone. |
| `changed-value` | An attribute value or a text changed. |
| `extra-element` | An element or text appeared. |
| `extra-attr` | An attribute appeared. |
| `part-missing` | A part is gone. |
| `part-extra` | A part appeared. |
| `part-bytes` | A non-XML part (or unparsable XML) changed. |
| `load-error` | `load_package` refused the file. |
| `panic` | The round trip panicked. |

Files are named `repo:<path>` for repo-tracked `.docx` and `ext:<path>` for the
external corpus, relative to the `FIDELITY_CORPUS` directory. So a local copy and
CI's checkout produce the same keys.

## Effective text

The gate also compares the effective text of `word/document.xml` before and
after the save (#1101). This is the text Word shows, with its `Range.Text`
characters for run content that is not `w:t`:

| Content | Character |
|---|---|
| `w:noBreakHyphen` | U+001E |
| `w:softHyphen` | U+001F |
| `w:tab`, `w:ptab` | tab |
| `w:br`, `w:cr` | U+000B (U+000C for a page break, U+000E for a column break) |
| `w:sym` | `(` |
| paragraph end | CR |

A character lost inside a restructured run shows up here even when the element
compare files it as one more run-restructuring entry.

A change in any file fails the gate, like a schema violation. It is never
baselined, and the report names the file, both lengths and the first
difference.

## Schema validation

A save can lose nothing and still write a file Word refuses as corrupt (#1083:
an unwrapped smart tag left its `w:smartTagPr` directly under `w:p`). So the
gate also validates every saved package against a subset of the
WordprocessingML schema (the transitional `wml.xsd` of ECMA-376 Part 4), in
`docxcore/tests/fidelity/schema.rs`. It needs no Word, .NET or XSD, and checks
a subset of what the Open XML SDK validator does.

Every part whose root element is in the `w:` namespace is checked: the
document, headers, footers, footnotes, endnotes, comments, numbering and
styles. These containers have a content model, wherever they occur:

- ordered sequences: `pPr`, `tblPr`, `tblPrEx`, `tcPr`, `sectPr`, `pBdr`,
  `pgBorders`, `tblBorders`, `tcBorders`, `tblCellMar`, `tcMar` (each side
  both physical and logical: `start` then `left`, `end` then `right`), `tbl`
  (only range markup before `tblPr`), `tr`;
- repeatable choices with an ordered head or tail: `rPr` (its properties come
  in any order and may repeat; a paragraph mark's `rPr` leads with its
  revision marks, and `rPrChange` is last) and `trPr` (row properties, then
  `ins`, `del`, `trPrChange`);
- allowed children: `body` (`sectPr` last), `p` (`pPr` first), `r` (`rPr`
  first), `hyperlink`, `smartTag` (`smartTagPr` first), `tc` (`tcPr` first,
  then at least one block), and the block containers `hdr`, `ftr`,
  `footnote`, `endnote`, `comment`, `txbxContent` and `docPartBody`.

A revision's snapshot of properties (`pPrChange/pPr`, `rPrChange/rPr`,
`tblPrChange/tblPr`, ...) has the narrower model the schema gives it: no
nested change, and a paragraph's snapshot ends at `cnfStyle`.

A child breaks one of four rules:

| Rule | Meaning |
|---|---|
| `not-allowed` | Its parent's model has no place for it. |
| `order` | It comes after a sibling the schema puts after it. |
| `duplicate` | It repeats where the schema allows one. |
| `missing` | A required child is absent (`tblPr`, `tblGrid`, a cell's block). |

Children in another namespace (extensions, DrawingML, math) are not checked,
but `w:` containers inside them are, so text box paragraphs count. An
`mc:AlternateContent` subtree is skipped.

The original package is validated too. A violation is counted by part, parent
path without indices, rule and child, and only the ones the save **added** fail:
a count above the original's. A non-conforming original therefore does not
fail the gate, and a violation the save fixed does not cover a new one. A
failure is reported as:

```
SCHEMA ext:table/2007.docx | word/document.xml | not-allowed | /w:document/w:body/w:p[39]/w:smartTagPr
```

Schema violations have no allowlist and no baseline: Word rejects the file
whether or not anything was lost, so any new one fails the gate, also under
`FIDELITY_UPDATE_BASELINE`. When one appears, fix the writer, or fix the table
in `schema.rs` if the schema allows the construct.

## Running it locally

```sh
corpus/tools/fetch-corpus.sh          # or fetch-corpus.ps1; populates corpus/files
cargo test -p docxcore --test fidelity -- --nocapture
```

The test always runs the repo-tracked `.docx`: `assets/`, `corpus/word-import/`,
`corpus/legacy/word/` and `docxcore/tests/fixtures/`. It also runs every `.docx`
under the docxy-corpus checkout, searched recursively.

| Variable | Effect |
|---|---|
| `FIDELITY_CORPUS=<dir>` | The docxy-corpus `files/` directory. The default is `corpus/files`; a relative path resolves against the workspace root. |
| `FIDELITY_REQUIRE_CORPUS=1` | Fail when that directory is missing. Without it the test prints a `SKIP external corpus` notice and runs the tracked files only. CI sets it. |
| `FIDELITY_UPDATE_BASELINE=1` | Rewrite `baseline.txt` from this run instead of judging against it (see below). |

`fetch-corpus.sh` shallow-clones [yeroo/docxy-corpus](https://github.com/yeroo/docxy-corpus)
and replaces `corpus/files/` and `corpus/xlsx-ext/` with its payload. Run it
again to update. When the clone fails (offline), it prints a SKIP notice, leaves
any existing copy alone and exits 0.

## The allowlist

`docxcore/tests/fidelity/allowlist.txt` lists the **deliberate, benign**
normalizations the gate tolerates in every file:

```
kind | part glob | index-free path glob | detail glob | [once-with <kind> <part> |] reason
extra-attr | word/document.xml | */w:t/@xml:space | preserve | The serializer writes xml:space="preserve" on every w:t. ...
```

In the globs, `*` matches any run of characters, including `/`. The detail is
what the report prints after the path:

| Finding | Detail |
|---|---|
| `lost-attr`, `extra-attr` | the attribute's decoded value, unquoted |
| `changed-value` | `"old" -> "new"`, each value Rust-Debug-quoted |
| `lost-element` / `extra-element` of text (`.../text()`) | the text, Rust-Debug-quoted |
| `lost-element` / `extra-element` of an element | its attributes as `name="value"` pairs, sorted by namespace URI and local name, values Rust-Debug-quoted (so `"` and `\` are escaped); empty for a mismatched root element |
| `part-bytes`, `load-error`, `panic` | free text (sizes, the error, the panic message) |
| `part-missing`, `part-extra` | empty |

So a rule can tolerate one specific element, such as the content-type
`Override` for an added styles part, rather than every extra element at that
path.

An optional `once-with <kind> <part> |` field before the reason narrows a rule
further. The rule then applies only in a file that also has a finding of that
kind in that part, and it absorbs one finding per file. The styles `Override`
rule uses it, so a duplicate Override of a styles part the file already had
still fails.

A rule without a reason is a parse error. The test prints how many
findings each rule absorbed.

Shrinking the allowlist is progress. Growing it needs review: a rule hides that
difference in **every** file, now and later. A real loss that is not fixed yet
belongs in the baseline, never here.

## The baseline

`docxcore/tests/fidelity/baseline.txt` holds the losses known today, one line
per file, part, kind and **index-free** path, tab-separated:

```
ext:table/Foo.docx	word/document.xml	lost-element	/w:document/w:body/w:tbl/w:tr/w:trPr/w:cantSplit
```

The baseline may only shrink:

- A finding not in the baseline fails (`NEW`).
- A baseline entry that no longer reproduces also fails (`STALE`): delete the
  line, so the fix cannot regress unseen.
- Only the files present in the run are judged. An entry for a file the run did
  not see is neither new nor stale.

Entries are grouped under **loss classes**. Each class names the issue that owns
its fix, and the classes are defined in `CLASSES` in `docxcore/tests/fidelity.rs`.
The gate also fails on an entry that no class claims. A new kind of loss
therefore needs a class and an issue, not just a line.

Classes match by path, which can mislabel one case. A run that #1069 splits or
unwraps can make the comparator pair the wrong runs, and then intact run
properties read as lost or changed. Such entries are listed one by one in
`MISALIGNED_RUNS`, each checked per character against the original, so the
#1069 class claims them instead of #1068. An exact list cannot claim a loss in
a file or at a path it does not name.

The baseline keys on the index-free path, so one line covers every occurrence of
that loss in that file. The trade-off: a new occurrence of an already listed loss
at another index in the same file is not reported. The finding that does get
reported always carries its full indexed path.

### Updating it

After a fix:

```sh
FIDELITY_UPDATE_BASELINE=1 cargo test -p docxcore --test fidelity round_trip_fidelity_gate
git diff docxcore/tests/fidelity/baseline.txt   # should only remove lines
```

The rewrite replaces the entries of every file in this run and keeps the
entries of files it did not see. So run it with the full corpus fetched, or a
partial run leaves stale lines for absent files behind for the next full run to
flag. A diff that **adds** lines is a regression, not an update, with one
exception. The comparator reports a difference once, at the root of the subtree
that differs. So when a fix stops losing a whole container (a `w:rPr`, an
attribute), any difference inside that node, or among runs aligned through it,
becomes visible for the first time. Such an entry may be added under its own
class, but only when an entry for the same file at that node or an ancestor goes
away in the same diff. The PR lists each one (#1063 did this).

## xlsx

`gridcore/tests/fidelity.rs` runs the same gate over `.xlsx`. It shares the
comparator, the allowlist and baseline formats, and the NEW / STALE /
UNCLASSIFIED rules: it includes `docxcore/tests/fidelity/mod.rs` by path, so
there is one comparator. Its own files are `gridcore/tests/fidelity/allowlist.txt`
and `gridcore/tests/fidelity/baseline.txt`, and its classes are `CLASSES` in
`gridcore/tests/fidelity.rs`.

```sh
corpus/tools/fetch-corpus.sh          # also populates corpus/xlsx-ext
cargo test -p gridcore --test fidelity -- --nocapture
```

- **The round trip** is `load_xlsx`, then `save_xlsx_for_path` with the file's
  own path: what xlsxy writes on save. It leaves out `stamp_save`, which
  rewrites `docProps` with the current time on purpose. xlsx has no separate
  no-edit save like docx's `save_package_preserving_document`: every save
  regenerates each worksheet's `<sheetData>`, `<cols>` and `<dimension>` from
  the model, so the gate measures that.
- **The files**: the repo-tracked `.xlsx` in `assets/`, `corpus/xlsx/`,
  `corpus/legacy/addin/`, `corpus/legacy/extra/`, `offxy-vscode/mcp/templates/`
  and `uiharness/fixtures/`, plus every `.xlsx` under `FIDELITY_XLSX_CORPUS`
  (default `corpus/xlsx-ext`, the docxy-corpus `xlsx-ext/` directory), searched
  recursively. `FIDELITY_REQUIRE_CORPUS` and `FIDELITY_UPDATE_BASELINE` work as
  for docx. `FIDELITY_UPDATE_BASELINE=1 cargo test --workspace` rewrites both
  baselines.
- **The sheet check.** A regenerated worksheet differs from its source in ways
  that change nothing a reader sees, and a structural finding cannot tell them
  from a loss. Examples: `0.14000000000000001` written `0.14`; an inline string
  moved to the shared strings; a shared formula expanded per cell; a dropped
  `<c r="B2"/>`; `customWidth="true"` written `1`. So each worksheet is also
  read the way a spreadsheet reads it. A worksheet is any part under
  `xl/worksheets/` or one the workbook's worksheet relationships name. Parts
  are keyed by their OPC name (an entry written `xl\sheet1.xml` is
  `xl/sheet1.xml`), and a part declaring ISO-8859-1 is read by its
  declaration, as the loaders read it (#1108).

  **Per cell** either side lists, it compares three things:
  - the **value**. Shared strings are found through the workbook's
    relationship. Shared and inline strings are resolved, run formatting
    included; phonetic runs are not read. Numbers compare as doubles, along
    with booleans and errors.
  - the **formula**: its text, plus kind and range for array and data-table
    formulas. A shared formula's follower is resolved from its master by
    shifting the master's relative references. The test does that with its
    own shifter, not gridcore's. A follower that has no master covering it
    does not resolve.
  - the **effective style**: the cell's own `s`, else its row's (with
    `customFormat`), else its column's.

  **Per column**, it compares the `<col>` attributes: width (as a double),
  `customWidth`, `style`, `hidden`, `bestFit`, `phonetic`, `outlineLevel` and
  `collapsed`.

  A difference is a `changed-value` finding at one of these paths:
  - `/cells/<ref>/value`
  - `/cells/<ref>/style`
  - `/cells/<ref>/formula-text`: the same formula in other words, meaning
    spaces, case or the `_xlfn.` prefix;
  - `/cells/<ref>/formula`: any other formula change, including a lost,
    added or unresolved formula;
  - `/cols/<letters or range>/<attribute>`: one path covers a run of
    adjacent columns that differ the same way.

  Each baseline line covers that cell or column run only. A new loss in
  another cell is NEW, and so is a changed formula filed where only
  re-serialization is baselined.

  Two more findings judge the saved worksheet alone, whatever the original
  said (#1152), and no loss class claims them, so they always fail:
  - `/cols/<letters or range>/no-width`: a `<col>` with no `width`, which
    Excel opens zero wide. The column comparison above reads it as no
    `<col>` at all, so it would miss one;
  - `/cols/<letters or range>/overlap`: a `<col>` starting at or before the
    end of the one before it (a duplicate, an overlap, or one out of order).

  The structural findings the check covers are dropped. Covered are:
  - all of `<cols>`;
  - a lost or extra `<c>` whose attributes are within `r`, `s` and `t` and
    whose children are `<v>`, `<f>` (with `t`, `si`, `ref`) and `<is>`
    (`<t>` and runs);
  - the `<v>` and `<f>` text;
  - the `<is>` text and runs;
  - the cell attributes `r`, `s` and `t`;
  - the formula attributes `t`, `si` and `ref`;
  - rows carrying nothing but `r` and `spans`.

  Everything else stays structural. That includes other row attributes,
  other cell attributes (`vm`, `cm`, `ph`), other formula attributes (`ca`,
  `aca`), phonetic runs, and a lost cell that carried any of them. Those
  baseline lines are index-free like docx's, so one line covers every
  occurrence in that sheet. The test prints how many findings the check
  covered.

- **Directory entries are no parts** (#1156). Some writers put directories
  in the ZIP as empty entries without a trailing `/` and mark them only in
  their ZIP attributes (tdf124525.xlsx has `_rels`, `docProps`, `xl`, ...).
  The loader and the comparator both treat an entry as a directory, not a
  part, when its name ends with `/`, or when it is empty and either another
  entry lies under it or its name has no extension. An empty part with an
  extension (`docProps/thumbnail.wmf`) is still a part. Before #1156 the
  save wrote those entries back as empty parts with no content type, and
  Excel opened the file with its repair prompt.

- **Package and schema validation** (#1156). Like docx's schema validation,
  every original and every save is validated, and a violation the save
  **added** fails the gate. Violations have no allowlist and no baseline,
  also under `FIDELITY_UPDATE_BASELINE`, because Excel repairs the file
  whether or not a value was lost. The checks live in
  `gridcore/tests/fidelity/schema.rs`.

  The package rules follow OPC (ECMA-376 Part 2). Part names, `Override`
  names and `Default` extensions compare ASCII case-insensitively.
  Relationship targets are percent-decoded and resolved against their
  source part, and `TargetMode="External"` targets are skipped. Which
  entries are parts is read from the ZIP central directory, as Excel reads
  it: an entry is a directory when its name ends with `/` or its attributes
  say so. The size rule above does not apply here, because an empty
  extensionless entry is exactly the part a save must not write.

  | Rule | Meaning |
  |---|---|
  | `missing-part` | No `[Content_Types].xml` or no `_rels/.rels`. |
  | `no-content-type` | A part with neither an `Override` nor a `Default` for its extension. |
  | `override-no-part` | An `Override` names no part. |
  | `duplicate-part` | Two entries name one part. |
  | `dangling-target` | An internal relationship targets no part. |

  The content models are a subset of the transitional `sml.xsd` of ECMA-376
  Part 4. They check the children of `worksheet` (CT_Worksheet order,
  `sheetData` required), `sheetData` (`row`), `row` (`c`, then `extLst`) and
  `workbook` (CT_Workbook order, `sheets` required), by name only. A
  violation uses docx's `not-allowed`, `order`, `duplicate` and `missing`
  rules, or `r-order`: a `row` or `c` whose `r` does not come after its
  previous sibling's. A row or cell without `r` is skipped. Children in
  another namespace (`mc:AlternateContent`, extensions) are not checked. A
  failure reads:

  ```
  SCHEMA ext:libreoffice/sc/qa/unit/data/xlsx/tdf124525.xlsx | xl | no-content-type | /xl
  ```

## What it does not cover
- Fixing the losses it found:
  - The unmodeled property children and attributes were fixed by #1063.
  - The other classes in `baseline.txt` each name their issue.
- Whether Word opens the file. Schema validation catches the structural part
  of that, by a subset of the schema. Attribute values, relationship targets
  and the containers it has no model for are not checked.
- Save paths other than `save_package` and `save_package_preserving_document`:
  the HTML bundle, Markdown, compare and merge, and the suite's unedited save
  `save_package_keeping_document` (#1107). That one is the preserving save
  plus `save_package`'s table-style pass, so it can add table styles to
  `styles.xml` and `[Content_Types].xml` where the preserving save does not.
- For xlsx:
  - attribute values, cell contents (`f`, `v`, `is`), and every part but the
    workbook and its worksheets; package and schema validation cover the
    package and those containers only;
  - other file types (`.xlsm`, `.xltx`, `.xlsb`), and Save As to another type;
  - what xlsxy does around a save, such as recalculation on open and
    `stamp_save`. Only the library's round trip is checked.
