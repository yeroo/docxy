# Docxy Office COM shims — VDI deployment package

Drop-in replacements for `Excel.Application` and `Word.Application` COM
automation, for machines that **do not have Microsoft Office installed**. Apps
that automate Office over COM — the driving case is **SLB Petrel's** "export to
Excel/Word" — keep working: the shims answer the COM traffic and write real
`.xlsx` / `.docx` files via the dependency-free `gridcore` / `docxcore` engines.

## What's in this folder

| File | Purpose |
|---|---|
| `xlcomshim.exe`, `xlcomshim.dll` | Excel shim — out-of-process server + in-process server |
| `wordcomshim.exe`, `wordcomshim.dll` | Word shim — out-of-process server + in-process server |
| `docxy-excel.tlb`, `docxy-word.tlb` | authored type libraries (for early-bound out-of-process marshalling) |
| `mktypelib.exe`, `mkwordtypelib.exe` | register/unregister the type libraries |
| `install.ps1` | register both shims, per-user (HKCU), both activation paths |
| `uninstall.ps1` | reverse it (removes our keys only) |
| `selftest.ps1` | prove both shims produce valid documents — needs no Office |

## Deploy

Copy this whole folder to the target and, in **any** PowerShell (no admin
needed — everything is per-user `HKCU\Software\Classes`):

```powershell
.\install.ps1        # add -Force to override a pre-existing HKCU Office mapping
.\selftest.ps1       # PASS/FAIL: creates a real .xlsx and .docx over COM
```

That's it. `CreateObject("Excel.Application")`, `new Excel.Application()`, the
Office PIA, and Petrel's exporter all now reach the shim.

To remove:

```powershell
.\uninstall.ps1
```

## How activation works (why both a .dll and a .exe)

Each shim registers **both** COM activation paths, on our own coclass **and** on
Office's real CLSID, plus the ProgIDs:

- **In-process** (`InprocServer32` → the `.dll`) — COM loads the DLL into the
  client's process and calls the vtable directly. No marshalling, no type
  library. `CLSCTX_SERVER` prefers this, so it's the usual path (and what makes
  early-bound work on a no-Office box without any typelib).
- **Out-of-process** (`LocalServer32` → the `.exe /automation`) — COM launches
  the EXE and marshals across the process boundary. Early-bound marshalling on a
  no-Office box needs a type library for the universal marshaller; the shipped
  `.tlb` files (proven ABI-identical to Office's, see the oracle tests) supply it.

Both late-bound (`IDispatch`) and early-bound (typed vtable) clients work — the
.NET Office PIA (Petrel's path), VBScript/`cscript`, and type-info-driven scripting
clients like **pywin32** (`win32com.client.Dispatch` / `gencache.EnsureDispatch`),
which the shims support by serving each object's real type information from the
bundled `.tlb`. If Python + pywin32 are on the target, the conformance tests under
`tools/comshim-tests/python/` and `tools/wordshim-tests/python/` exercise that path.

## Nothing is stomped

- **Per-user only.** Writes `HKCU\Software\Classes`; never `HKLM`.
- **Guarded.** `install.ps1` refuses to overwrite a *different* existing HKCU
  mapping unless `-Force`.
- **Reversible.** `uninstall.ps1` removes the Office-CLSID shadow and ProgID
  mappings only where they still point at our shim.
- An actually-installed Office (HKLM) is never touched. If Office is present, the
  in-process shim shadows it per-user for the current user only.

## When something misbehaves in the field

Every COM call is logged. Read the trace:

```
%TEMP%\xlcomshim.log
%TEMP%\wordcomshim.log
```

Unmodeled late-bound members degrade gracefully (a property put is swallowed; a
get/call returns a do-nothing object so chains keep flowing) rather than
faulting the host — so an export never aborts on an unforeseen member; it logs.

## Regenerating the type libraries

The `.tlb` files are authored by reading Office's own type library, so they can
only be **generated** on a machine that has Excel/Word — which is why they're
shipped as committed artifacts. To regenerate:

```powershell
mktypelib.exe docxy-excel.tlb        # on a machine with Excel
mkwordtypelib.exe docxy-word.tlb     # on a machine with Word
```

Registration itself needs no Office and happens on the target.
