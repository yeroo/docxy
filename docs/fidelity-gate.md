# The docx round-trip fidelity gate

Saving must never silently destroy a user's document (#1060). The fidelity gate
opens every corpus `.docx`, saves it with no edits, and compares every package
part with the original. A loss nobody has accepted fails the build.

It is a plain cargo test, `docxcore/tests/fidelity.rs`, and the `docx fidelity
gate` job runs it in CI on every PR.

## What it checks

The gate round-trips each file the way docxy saves an **edited** document:
`load_package`, then `Editor::new`, then `save_package`. That save regenerates
`word/document.xml` from the semantic model, so it is where untouched
paragraphs of an edited document lose what the model does not understand.

A user who saves **without** editing gets `save_package_preserving_document`,
which writes the original parts back. The gate asserts that path is
byte-identical part for part. That check has no baseline: any difference fails
outright. "The gate reports a loss in X" therefore means "X is lost once the
user edits anything in the document". It does not mean "X is lost by
open+save".

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
flag. A diff that **adds** lines is a regression, not an update.

## What it does not cover

- xlsx: #1064.
- Fixing the losses it found:
  - The unmodeled property children and attributes are #1063.
  - The other classes in `baseline.txt` each name their issue.
- Save paths other than `save_package` and `save_package_preserving_document`:
  the HTML bundle, Markdown, compare and merge.
