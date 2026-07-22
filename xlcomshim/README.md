# xlcomshim — an Excel-compatible COM automation server

A Windows **COM LocalServer32** that impersonates `Excel.Application`, so
software that automates Office over COM (create a workbook, write cells, save an
`.xlsx`) keeps working on machines that **do not have Microsoft Excel installed**.
Document output is produced by the dependency-free [`gridcore`](../gridcore)
engine — the same one behind `xlsxy`.

The driving use case is **SLB Petrel**, whose "export to Excel" path drives a
live Excel instance via COM and fails when Excel is absent. This shim registers
`Excel.Application` in the user's registry and answers that COM traffic itself.

## How it works

- Registered (per-user, `HKCU\Software\Classes`) so the `Excel.Application`
  ProgID resolves to **our own coclass GUID** — never Microsoft's Excel CLSID.
- COM launches the EXE as a `LocalServer32` (`-Embedding`). It registers a class
  factory and serves an `IDispatch` object graph
  (`Application → Workbooks → Workbook → Worksheets → Worksheet → Range`) whose
  member ids are **Excel's real DISPIDs** (verified against the installed Excel
  type library). Out-of-process means COM marshals across the 32/64-bit boundary
  for free, so one 64-bit server drives 32-bit clients too.
- Every activation, name lookup, and dispatch is written to
  `%TEMP%\xlcomshim.log` — this is the **field diagnostic**: run a real client
  (e.g. Petrel) against the shim and the log shows exactly which members it
  called and whether it bound late (`IDispatch`) or early (vtable/typelib).

## Status

| Phase | What | State |
|---|---|---|
| **P0** | Prove COM launches the server + routes a late-bound call end-to-end, with logging | ✅ **done** — verified with a VBScript client (`Name`/`Version`/`Visible`/`Workbooks`/`Quit`) |
| **P1** | Full `create → write → SaveAs → quit`, backed by `gridcore`, verified against real Excel as the oracle | ✅ **done** — `tools/comshim-tests/verify.ps1`: the shim creates an `.xlsx` (`Workbooks.Add`, `Worksheets(1)`, `Range`/`Cells` writes, `=SUM` formula, `SaveAs 51`); **real Excel opens it with no repair** and recomputes `B4=42.5` |
| P1.5 | In-binary `/regserver`, number formats + basic Font/Interior, array (range) writes, `.xls` | next |
| P2 | Early-bound typelib/vtable — only if a client needs it (the log decides) | gated |
| P3 | Petrel integration on the VDI | pending |
| P4 | `Word.Application` over `docxcore` | later |

## Try it (P0)

Real Excel, if installed, is **not** touched — registration is per-user and
fully reversible.

```powershell
cargo build --release -p xlcomshim
tools\comshim\register-shim.ps1 -Force        # map Excel.Application -> the shim (HKCU)

cscript //nologo - <<'VBS'
Set x = CreateObject("Excel.Application")
WScript.Echo "Version=" & x.Version
x.Quit
VBS

tools\comshim\unregister-shim.ps1             # restore (real Excel visible again)
type %TEMP%\xlcomshim.log                      # the dispatch trace
```

## Safety / registration

`register-shim.ps1` writes only `HKCU\Software\Classes` (never `HKLM`), uses a
distinct shim CLSID, and refuses to overwrite a different existing mapping unless
`-Force`. An elevated/service client bypasses `HKCU`, so an opt-in machine-wide
mode will come with P1's in-binary `/regserver`.

Reference: the full Excel object-model spec (IIDs, DISPIDs, vtable order, enum
values) was extracted from the live Excel type library; the DISPIDs seeded here
come from that dump.
