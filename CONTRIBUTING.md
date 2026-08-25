# Contributing to Docxy

Thanks for your interest in improving Docxy!

## Building

```
cargo build --release
```

Produces `target/release/docxy`.

### Two workspaces

The repo holds **two** cargo workspaces: the root one, and `suite/` — the GPUI
desktop app, kept separate because it pins `gpui` to a git revision through its
own `Cargo.lock` and its own `[workspace.lints]`. They share the `*core` crates
by path.

That split is load-bearing: a change to a `gridcore` / `docxcore` public type can
compile green in one workspace while breaking the other, because only the root
one builds `xlsxy`, `gridwasm` and the TUIs against those types. **Build and test
both** whenever you touch a core crate's public API. CI (`.github/workflows/ci.yml`)
only covers the root workspace, so the suite's tests are on you.

## Testing

```
cargo test --workspace
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

cargo test --manifest-path suite/Cargo.toml
cargo clippy --manifest-path suite/Cargo.toml --all-targets -- -D warnings
```

The desktop suite's grid, its range-selector fields and the two GPUI traps they
rest on are written up in
[`suite/docs/range-selector.md`](suite/docs/range-selector.md); how a chart
reads its range — each column a series, or each row — is in
[`suite/docs/chart-orientation.md`](suite/docs/chart-orientation.md), and why a
pie keeps every series it holds while plotting the first is in
[`suite/docs/pie-series.md`](suite/docs/pie-series.md).

The root workspace is twenty crates. Layered bottom-up, at its core:

- **`opccore`** — pure, `std`-only OPC container plumbing (ZIP read/write,
  DEFLATE, XML pull parser) shared by both document formats.
- **`docxcore`** — pure, `std`-only DOCX I/O (the Word document model,
  rendering, and the PDF writer) on top of `opccore`.
- **`gridcore`** — pure, `std`-only XLSX engine (workbook model, lossless
  I/O, and the formula/recalculation engine) on top of `opccore`.
- **`docxy`** — the `.docx` terminal UI (ratatui), clipboard, image rendering.
- **`xlsxy`** — the `.xlsx` terminal UI.

The rest follow the same shape — a pure core plus its front end: `projcore` /
`yppxy` / `mppread` (project schedules), `mailcore` / `lookxy` (mail and
calendar), `editcore`, `ribboncore` / `ribbonspec` / `backstagecore` and
`ctlcore` (shared UI and the agent control surface), `docxwasm` / `gridwasm`
(the browser builds), and `comshimcore` / `xlcomshim` / `wordcomshim` (the COM
shims). The desktop GPUI suite lives in its own workspace under `suite/`.

The three `*core` crates above must stay **dependency-free** (`std` only), as
must `projcore` and `editcore`; `mailcore` is the exception, since IMAP and a
local mail store need HTTP/TLS and SQLite. Most logic
lives there and is covered by fast, pure unit tests — please add tests there for
behavior changes.

## Coverage

Line/region coverage is measured with
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) and reported to
[Codecov](https://codecov.io/gh/yeroo/docxy) on every push (see the badge in the
README). To measure locally:

```
cargo install cargo-llvm-cov          # one-time
cargo llvm-cov --workspace            # summary table in the terminal
cargo llvm-cov --workspace --html     # browsable report under target/llvm-cov/html
```

New behavior should come with tests; coverage is informational and never gates a
merge, but a PR that drops it noticeably is worth a second look.

## Benchmarks

Performance-sensitive hot paths (DEFLATE, DOCX load/render/PDF, the XLSX calc
engine and I/O) have [Criterion](https://github.com/bheisler/criterion.rs)
benchmarks. They're gated behind a `bench` feature so the normal and MSRV builds
never compile Criterion:

```
cargo bench -p opccore  --features bench            # inflate + zip
cargo bench -p docxcore --features bench            # load / serialize / render / pdf
cargo bench -p gridcore --features bench            # recalc / xlsx I/O / parse
```

Criterion prints per-benchmark timings (and flags regressions vs. the previous
run stored under `target/criterion`). A quick smoke run:
`cargo bench -p gridcore --features bench --bench engine -- --measurement-time 1 --sample-size 10`.

## Guidelines

- Format with `cargo fmt` (rustfmt defaults); keep `clippy` clean.
- Keep the **`*core` crates dependency-free** — runtime crates (ratatui,
  clipboard, image, …) belong only in the `docxy`/`xlsxy` frontends.
- Keep changes focused; one logical change per pull request.
- If you change behavior, describe it in the PR (and update the README if it is
  user-facing).

## Reporting issues

Open an issue with the exact command you ran and, if possible, a minimal `.docx`
that reproduces the problem. For image-rendering issues, include your terminal.
