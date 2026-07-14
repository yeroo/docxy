# docxy comparison corpus

Test workbooks/documents used to eyeball `docxy`/`xlsxy` against Word and Excel
and to drive the corpus verify sweeps.

## What lives here (tracked)

- `xlsx/` — the **first-party synthetic oracle**: 17 small hand-authored `.xlsx`
  whose expected results are known. The `gridcore` conformance test
  (`gridcore/tests/conformance.rs`) recalculates these and diffs against the
  oracle, so they ship with the repo.
- `tools/classify.py`, `tools/classify_xlsx.py` — regenerate the manifests by
  scanning each file's parts/XML.

## What lives in the separate corpus repo

The large third-party binary corpus (~14 MB) was moved to
[**github.com/yeroo/docxy-corpus**](https://github.com/yeroo/docxy-corpus) so it
doesn't bloat this repo's history. It is **not** needed to build or test the
crates — only the local `compare/` / `compare-xlsx/` launchers and the verify
sweeps use it. These paths are git-ignored here; populate them from that repo:

```sh
# from the root of a docxy checkout
git clone https://github.com/yeroo/docxy-corpus /tmp/docxy-corpus
cp -r /tmp/docxy-corpus/files    corpus/files          # OpenXML SDK .docx (MIT)
cp -r /tmp/docxy-corpus/xlsx-ext corpus/xlsx-ext        # LibreOffice+OOo .xlsx
cp    /tmp/docxy-corpus/*.json   corpus/                # manifests
```

| Path | Source | License |
|---|---|---|
| `files/` | OpenXML SDK test assets (dotnet/Open-XML-SDK) | MIT |
| `xlsx-ext/libreoffice/` | LibreOffice `sc`/`chart2`/`oox` QA | MPL-2.0 |
| `xlsx-ext/openoffice/` | Apache OpenOffice test data | Apache-2.0 |
| `classification.json` / `classification-xlsx.json` | generated manifests | — |

After adding files, regenerate a manifest with e.g.
`python corpus/tools/classify_xlsx.py`. The compare launchers resolve each file
by its manifest path, so nothing else changes.
