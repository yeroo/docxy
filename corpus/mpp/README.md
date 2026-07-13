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

Both `git clone` and `raw.githubusercontent.com` downloads work here (only the
GitHub *API* and `gh` are gated). Because the API is gated there's no directory
listing over HTTP, so the reliable way to discover `.mpp` files in a repo is a
blobless clone and a tree walk:
```
git clone --depth 1 --filter=blob:none --no-checkout https://github.com/<owner>/<repo>.git r
git -C r ls-tree -r --name-only HEAD | grep -i '\.mpp$'
```
then fetch the ones you want with `raw.githubusercontent.com` (URL-encode
spaces as `%20`).

Verified working sources (all real OLE2/CFB `.mpp`), spanning Project versions:

- **ProjectLibre samples** (`cyclingzealot/projectlibre-jlam`) — a
  Commercial-Construction plan (MPP9), an MS-Project-2003 deployment plan (MPP9,
  323 tasks), and the classic *New Product* template (Project 98 / MPP8).
- **Software-project coursework** (`saswat3348/Project-Management`) — MPP14.
- **Azure ML Data Science** (`Azure-Samples/Azure-MachineLearning-DataScience`)
  — an "Advanced Analytics" plan in a newer MPP format.
- **MPXJ** (`github.com/joniles/mpxj`, Apache-2.0) — the docs (`doc/MPP8.xls` …
  `doc/MPP14.xls`) map the binary layout field-by-field. Note its regression
  `.mpp` corpus is **not** in the repo — the build takes it from an external
  `-Dmpxj.junit.datadir` — so those files aren't fetchable from the repo.

Fetch a single file into this (git-ignored) folder, e.g.:
```
curl -sSL -o corpus/mpp/construction.mpp \
  "https://raw.githubusercontent.com/cyclingzealot/projectlibre-jlam/master/openproj_build/resources/samples/Commercial%20construction%20project%20plan.mpp"
```

### Known decode gaps (good reverse-engineering targets)

The current decoder handles MPP9 and MPP12/14 (names, dates, outline, links) —
including a *New Product* template that Project 98 wrote and a later version
re-saved as MPP9, which the link oracle (below) now dates correctly, and the
newest MPP generation (the Azure "Advanced Analytics" plan) for **names and
dates**. That newest file needed two `VarMeta`/`Var2Data` fixes:

- Its `Var2Data` isn't one contiguous run of length-prefixed blocks (gaps +
  reordering), so the sequential walk stalled at ~1 name. Name blocks are now
  read *directly at each `VarMeta` offset* — the authoritative index.
- Its `VarMeta` entry is a third shape (`[field:u16][0x0B40][item:u32]
  [offset:u32]`, field at offset−8), and a stray one-char marker field shares
  the constant `0x0B40` slot. The name field is now chosen as the *purest*
  mostly-multi-char field, so the marker can't merge into it.

Still open on that newest file: its **outline and link tables** use layouts not
yet reversed (the decoder declines both rather than guessing).

The **link oracle**: a `.mpp` record holds several date-like field pairs
(Start/Finish, but also baseline/actual/early/late/constraint dates), and more
than one can satisfy `start ≤ finish`, so that test alone occasionally locks
onto the wrong pair (the *New Product* file decoded 2011 finishes for a 2004
plan). When the `TBkndCons` links are present, the decoder now uses them to
break the tie: the real Start/Finish pair is the one under which the
Finish-to-Start links hold. It's trusted only when it clearly applies (≥half the
links map in range, ≥90% consistent), else it falls back to the plain most-valid
pair — so sparse-uid files (where uid≠row) are unaffected.

## What already works on a real .mpp

```
cargo run -p mppread --example streams  -- corpus/mpp/x.mpp                 # metadata + stream map
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp                 # full storage-tree paths
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp "   1/TBkndTask/Var2Data"          # hex
cargo run -p mppread --example inspect  -- corpus/mpp/x.mpp "   1/TBkndTask/Var2Data" strings  # task NAMES
yppxy corpus/mpp/x.mpp                                                       # opens with the .mpp title
```

The container (CFB), the storage tree, the metadata (property sets), the
**task names**, and each task's **start/finish dates** decode from real files
(MPP9 + MPP12/14, auto-detected):

```
cargo run -p mppread --example tasknames -- corpus/mpp/x.mpp   # names + start/finish
```

Dates, **outline levels**, and **predecessor links** come from the per-task
`FixedData` records and the sibling `TBkndCons` table: the record size and
date-field offset are auto-detected as the layout under which every task's
`start ≤ finish` and the starts vary; the outline column is found by MS
Project's tree rule (depth deepens by ≤1 per row and pops back up at hierarchy
boundaries); links map task unique-ids to rows via the uid column under which
the most Finish-to-Start links respect the decoded dates — the same
self-validating approach as the name decode throughout. So `yppxy
corpus/mpp/x.mpp` now opens with the real WBS tree, schedule, **and** dependency
network, not just the metadata.

What remains is link **lag** (0 throughout the corpus, so unvalidated) — best
reversed against an MSPDI oracle export of a project that uses lead/lag.
`mppread/tests/real_mpp.rs` locks in the name, date, outline, and link decode
against local sample files (and skips when they're absent).
