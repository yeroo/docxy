# xlcomshim — an Excel-compatible COM automation server

A Windows COM server that impersonates `Excel.Application`, so software that
automates Office over COM (create a workbook, write cells, format them, save an
`.xlsx`) keeps working on machines that **do not have Microsoft Excel installed**.
Document output is produced by the dependency-free [`gridcore`](../gridcore)
engine — the same one behind `xlsxy`.

The driving use case is **SLB Petrel**, whose "export to Excel" path drives a
live Excel instance via COM and fails when Excel is absent. This shim registers
`Excel.Application` and answers that COM traffic itself, backed by gridcore.

## How it works

It registers (per-user, `HKCU\Software\Classes`, never `HKLM`) under **our own
coclass GUID** — never Microsoft's Excel CLSID — and serves the full object graph
`Application → Workbooks → Workbook → Worksheets → Worksheet → Range` (plus
`Font`/`Interior`) with **Excel's real IIDs and DISPIDs**, verified against the
installed Excel type library.

It answers **both** binding styles a .NET client (like Petrel) can use:

- **Late-bound** (`IDispatch::Invoke`) — `CreateObject`, VBScript, C# `dynamic`.
- **Early-bound** (typed vtable) — the Office PIA. Our objects implement Excel's
  dual interfaces in exact vtable order (generated from the typelib), so a
  `(Excel.Application)` cast and typed member calls succeed. The subtlety that
  makes this work is the hidden **`[lcid]`** argument the CLR injects for many
  members — our vtable signatures include it at the exact position the PIA's
  `[LCIDConversion]` metadata dictates.

Two activation paths, so it works however the host activates COM:

- **In-process** (`InprocServer32`, `xlcomshim.dll`) — COM loads the DLL into the
  client's process and calls our vtable directly. **No marshalling, no type
  library.** `CLSCTX_SERVER` prefers in-proc, so this is the common path.
- **Out-of-process** (`LocalServer32`, `xlcomshim.exe`) — COM launches the EXE
  and marshals across the boundary. On a no-Office machine that needs a type
  library for the universal marshaller to build vtable proxies, so we ship and
  register our own (`docxy-excel.tlb`, authored from Excel's typelib and proven
  ABI-identical by `tests/typelib_faithful.rs`).

**Never faults on an unmodeled member.** The strategy is cover-broad, log every
call, deploy, read the log. So an unknown late-bound member does not error (which
would abort the host's export) — it logs and degrades: a property put is
swallowed, a get/call returns a do-nothing object so chains like
`range.Font.Bold = True` keep flowing. Every call is written to
`%TEMP%\xlcomshim.log` — the field diagnostic.

## Status

| Area | State |
|---|---|
| **Activation** | ✅ late-bound (IDispatch) **and** early-bound (typed vtable), both in-process (DLL) and out-of-process (EXE) |
| **Create path** | ✅ `Workbooks.Add` → `Worksheets(n)` → `Range`/`Cells`/`Offset`/`Resize` writes → `=SUM` formulas → `SaveAs`; real Excel opens the result with no repair |
| **Formatting** | ✅ Font bold/italic/color, Interior fill, NumberFormat, HorizontalAlignment — written into `styles.xml`, rendered by real Excel |
| **Robustness** | ✅ unmodeled members degrade gracefully + are logged; unmodeled early-bound slots log their interface+slot before a clean `E_NOTIMPL` |
| **Type library** | ✅ authored `docxy-excel.tlb` for the no-Office out-of-process path; oracle differential test proves it matches Excel (939 methods, 2067 params) |
| Word | ⬜ `Word.Application` over `docxcore` — later |

Known limits: graceful degradation covers the late-bound path; an unmodeled
*early-bound* member returns a clean `E_NOTIMPL`. Whole-column font/fill
formatting isn't represented (the per-cell style model). `Range` dispatches
through `IDispatch` by design — it is a **dispinterface** in Excel's object model,
so even real Excel serves Range only via `IDispatch` (`(Excel.IRange)range`, the
vtable IID, throws `InvalidCastException` against Excel itself). Our Range path
matches Excel exactly; there is no vtable Range to implement.

## Try it

Real Excel, if installed, is **not** touched — registration is per-user and
fully reversible.

```powershell
cargo build --release -p xlcomshim         # builds xlcomshim.exe, xlcomshim.dll, mktypelib.exe

# --- out-of-process (also registers the typelib) ---
tools\comshim\register-shim.ps1 -Force
cscript //nologo tools\comshim-tests\graceful-smoke.vbs %TEMP%\out.xlsx
tools\comshim\unregister-shim.ps1

# --- in-process (no typelib needed; the likely Petrel path) ---
tools\comshim\register-inproc.ps1 -Force
#   ... run an early-bound client; it loads xlcomshim.dll in-process ...
tools\comshim\unregister-inproc.ps1

type %TEMP%\xlcomshim.log                    # the dispatch trace / field diagnostic
```

### Deploying where Office is absent (the VDI)

`mktypelib` authors the `.tlb` by reading Excel's own typelib, so it can only be
**generated** on a machine with Excel — the produced `tools/comshim/docxy-excel.tlb`
is committed as a shipped artifact. Ship the release output
(`xlcomshim.exe`, `xlcomshim.dll`, `mktypelib.exe`), the `tools/comshim` scripts,
and that `.tlb`; on the target, `register-shim.ps1` / `register-inproc.ps1` do the
rest (they only *register*, which needs no Excel).

## Tests

- `tools/comshim-tests/verify.ps1` — create via the shim, then real Excel opens
  it and recomputes the `=SUM` (the oracle).
- `tools/comshim-tests/graceful-smoke.vbs` — hammers unmodeled members +
  navigation writes + formatting; must not fault and must produce a valid file.
- `tools/comshim-tests/csharp/` — the .NET PIA harness (`castshim` early-bound
  out-of-process, `castinproc` in-process, `late` late-bound).
- `cargo test -p xlcomshim` — the typelib oracle differential test.

## Safety / registration

All registration is per-user (`HKCU\Software\Classes`), uses a distinct shim
CLSID, is guarded (refuses to overwrite a different existing mapping without
`-Force`), and is fully reversible. `HKLM` and an installed Excel are never
touched.
