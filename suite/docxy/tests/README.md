# docxy UI integration tests

`ui_e2e.ps1` drives the **real** built `suite.exe` through simulated
keyboard/mouse input and asserts on the document it saves — a genuine
end-to-end check of the UI event path (key routing, ribbon/KeyTips actions,
header/footer editing, tab stops, …).

## Why not `cargo test` / gpui `TestAppContext`?

gpui ships a headless test harness (`TestAppContext`), but it can't be used for
this crate: compiling `suite` under `--test` with `gpui/test-support` triggers an
unbounded macro expansion against the app's very large builder-chain `render`
function (rustc runs out of stack / recursion limit and never converges). So the
integration tests live outside the crate as this driver instead. Pure engine
behaviour is covered by the 434 `cargo test` unit tests in `docxcore`.

## Running

Windows only (needs an interactive desktop session — it moves the real cursor
and types real keys, so don't touch the machine while it runs):

```powershell
# build first, then run
cargo build --manifest-path suite/Cargo.toml
pwsh suite/docxy/tests/ui_e2e.ps1        # or: powershell -File suite/docxy/tests/ui_e2e.ps1
```

Exits `0` if every scenario passes, `1` otherwise. Each scenario prints
`PASS`/`FAIL` with a short reason.

## What it covers

Each scenario seeds a session pointing at a **minimal, entirely plain** `.docx`
(so any marker in the saved file can only come from the action under test — never
a false positive from pre-existing content), launches the app, drives it, saves
(Ctrl+S), and inspects the saved parts:

| Scenario      | Drives                                   | Asserts |
|---------------|------------------------------------------|---------|
| `tab`         | Tab key ×2 at line start                 | two `<w:tab/>` |
| `bold`        | Ctrl+A, Ctrl+B                           | `<w:b/>` |
| `italic`      | Ctrl+A, Ctrl+I                           | `<w:i/>` |
| `header`      | Insert ▸ Edit Header (KeyTips), type, Esc | `header1.xml` contains the text |
| `first-page`  | Edit Header ▸ Different First Page toggle | `<w:titlePg/>` |
| `line-spacing`| Home ▸ Line Spacing menu ▸ 1.5×          | `w:line="360"` |
| `page-number` | Insert ▸ Page Number (KeyTips)           | `w:instr="PAGE"` field |
| `no-spacing`  | Home ▸ No Spacing style (gallery)        | `w:line="240"` (single) |
| `symbol`      | Insert ▸ Symbol (KeyTip) ▸ pick em dash  | em dash in the text |

## Gotchas baked into the harness

- **`session.json` must be UTF-8 without a BOM.** A BOM makes serde reject the
  JSON and the app silently falls back to its built-in sample document — which
  already contains bold/italic/em-dash and would make several assertions pass
  falsely. The harness writes it BOM-free via `UTF8Encoding($false)`.
- Window layout is the app's default 1180×800; click coordinates assume it.
- `Set-Content -Encoding utf8` on Windows PowerShell 5.1 adds a BOM — don't use
  it for `session.json`.
