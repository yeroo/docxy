# wordcomshim — a Word-compatible COM automation server

A Windows COM server that impersonates `Word.Application`, so software that
automates Office over COM (create a document, type text, save a `.docx`) keeps
working on machines that **do not have Microsoft Word installed**. Document
output is produced by the dependency-free [`docxcore`](../docxcore) engine — the
same one behind `docxy`. This is the Word counterpart to
[`xlcomshim`](../xlcomshim).

## How it works

Registered per-user (`HKCU\Software\Classes`, never `HKLM`) under **our own
coclass GUID**, so `Word.Application` resolves to the shim. It serves the
late-bound (`IDispatch`) object graph
`Application → Documents → Document → Selection / Range`, backed by docxcore:

- `Documents.Add` / `Documents.Open(path)` → a document,
- `Selection.TypeText` / `TypeParagraph`, `Range.Text` / `InsertAfter` /
  `InsertParagraphAfter`, `Document.Content` → build the body,
- `Document.SaveAs2(path)` → a real `.docx` via `docxcore::package::save_package`.

Two activation paths (as in xlcomshim): out-of-process (`LocalServer32`,
`wordcomshim.exe`) and in-process (`InprocServer32`, `wordcomshim.dll`). Unmodeled
members degrade gracefully — a put is swallowed, a get/call returns a do-nothing
object — so nothing faults, and every call is logged to `%TEMP%\wordcomshim.log`.

## Status

| Area | State |
|---|---|
| Late-bound create path | ✅ `Documents.Add` → `Selection`/`Range` text → `SaveAs2`; **real Word opens the result** and reads every paragraph back |
| Both activation paths | ✅ LocalServer32 (.exe) and InprocServer32 (.dll) |
| Graceful degradation | ✅ unmodeled members logged + benign |
| Early-bound (typed vtable + typelib) | ⬜ next — mirror xlcomshim's dual-interface + `mktypelib` approach against Word's typelib |
| Formatting (Font/ParagraphFormat) | ⬜ later — over docxcore run/paragraph props |

## Try it

Real Word, if installed, is **not** touched — registration is per-user and
reversible.

```powershell
cargo build --release -p wordcomshim
tools\wordshim\register-word.ps1 -Force
cscript //nologo tools\wordshim-tests\word-smoke.vbs %TEMP%\out.docx
tools\wordshim\unregister-word.ps1
type %TEMP%\wordcomshim.log
```

## Safety / registration

All registration is per-user (`HKCU`), uses a distinct shim CLSID
(`{9C2F4A10-…}`, never Microsoft's Word CLSID `{000209FF-…}`), is guarded against
overwriting a different mapping without `-Force`, and is fully reversible.
