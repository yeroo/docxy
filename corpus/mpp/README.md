# .mpp decode corpus (bring your own files)

Sample Microsoft Project **binary** `.mpp` files for developing and validating
the `mppread` decoder. **None are committed** — `.mpp` files are third-party
binaries with their own licensing, and they're large — so this folder ships
only this README and a `.gitignore` that keeps any `.mpp` you drop here out of
git.

## Why a real corpus is needed

Unlike MSPDI XML (documented) and `.xlsx` (documented), a `.mpp`'s
task/resource data lives in **undocumented, version-specific** var-data blocks
(MPP8 / MPP9 / MPP12 / MPP14, one per Project generation). There is no spec to
implement against — the layout has to be reverse-engineered against real files
with known contents, exactly the oracle-scoreboard method used for the xlsx
corpus. What *is* documented (and already decoded) is the container (MS-CFB)
and the metadata property sets (MS-OLEPS).

## Workflow for adding the task decoder

1. Drop a few `.mpp` files here, ideally spanning Project versions and with the
   same schedule saved *both* as `.mpp` and as MSPDI `.xml` (File ▸ Save As ▸
   XML). The MSPDI export is the **oracle** — the known-good answer.
2. Map each file's storage tree:
   ```
   cargo run -p mppread --example streams  -- corpus/mpp/yourfile.mpp
   cargo run -p mppread --example inspect  -- corpus/mpp/yourfile.mpp
   ```
   These print the metadata plus every stream's **full path** through the
   storage hierarchy (`\x05SummaryInformation`, `TBkndTask/FixedData`,
   `TBkndTask/Var2Data`, the resource/calendar blocks, …). Then hex-dump a
   specific block to eyeball its header:
   ```
   cargo run -p mppread --example inspect  -- corpus/mpp/yourfile.mpp TBkndTask/FixedData
   ```
3. Reverse the fixed/var-data blocks field by field, checking each decoded task
   date/duration/link against the MSPDI oracle for the same file, until a
   `corpus/mpp` scoreboard reads green — then wire the decoder into
   `mppread` and expose `.mpp → projcore::Project` so `yppxy file.mpp` opens
   the real schedule (today it opens the metadata only).

## Where to get sample files

`raw.githubusercontent.com` downloads work here (the GitHub *API* is gated).
Verified working sources (all real OLE2/CFB `.mpp`):

- **MPXJ** (`github.com/joniles/mpxj`, Apache-2.0) — the richest source: a large
  regression suite across every Project version, many paired with expected
  values (an oracle). Its `doc/MPP8.xls` … `doc/MPP14.xls` document the binary
  layout field-by-field — the reverse-engineering map.
- Individual sample projects on GitHub, e.g. a Commercial-Construction plan
  (ProjectLibre samples) and MS-Project software-project files.

Fetch into this (git-ignored) folder, e.g.:
```
curl -sSL -o corpus/mpp/construction.mpp \
  "https://raw.githubusercontent.com/cyclingzealot/projectlibre-jlam/master/openproj_build/resources/samples/Commercial%20construction%20project%20plan.mpp"
```

## What already works on a real .mpp

```
cargo run -p mppread --example streams  -- corpus/mpp/x.mpp                 # metadata + stream map
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp                 # full storage-tree paths
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp "   1/TBkndTask/Var2Data"          # hex
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp "   1/TBkndTask/Var2Data" strings  # task NAMES
yppxy corpus/mpp/x.mpp                                                       # opens with the .mpp title
```

The container (CFB), the storage tree, the metadata (property sets), and the
**task names** (Var2Data UTF-16 blocks) already decode from real files. What
remains is the numeric task data (dates, durations, links) in the Fixed/Var
data blocks, keyed by `VarMeta` and the version-specific field ids from the
MPP*.xls docs — validated against an MSPDI oracle export of the same project.
