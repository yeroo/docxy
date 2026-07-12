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
2. Map each file's streams:
   ```
   cargo run -p mppread --example streams -- corpus/mpp/yourfile.mpp
   ```
   This prints the metadata plus the stream directory (`Props`, `Var2Data`,
   `Fixed2Data`, the task/resource/calendar blocks, …).
3. Reverse the fixed/var-data blocks field by field, checking each decoded task
   date/duration/link against the MSPDI oracle for the same file, until a
   `corpus/mpp` scoreboard reads green — then wire the decoder into
   `mppread` and expose `.mpp → projcore::Project` so `yppxy file.mpp` opens
   the real schedule (today it opens the metadata only).

## What works today without a corpus

```
cargo run -p mppread --example streams -- any.mpp   # metadata + stream map
yppxy any.mpp                                        # opens with the .mpp title
```
