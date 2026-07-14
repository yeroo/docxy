# Contributing to Docxy

Thanks for your interest in improving Docxy!

## Building

```
cargo build --release
```

Produces `target/release/docxy`.

## Testing

```
cargo test --workspace
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

The workspace is five crates, layered bottom-up:

- **`opccore`** — pure, `std`-only OPC container plumbing (ZIP read/write,
  DEFLATE, XML pull parser) shared by both document formats.
- **`docxcore`** — pure, `std`-only DOCX I/O (the Word document model,
  rendering, and the PDF writer) on top of `opccore`.
- **`gridcore`** — pure, `std`-only XLSX engine (workbook model, lossless
  I/O, and the formula/recalculation engine) on top of `opccore`.
- **`docxy`** — the `.docx` terminal UI (ratatui), clipboard, image rendering.
- **`xlsxy`** — the `.xlsx` terminal UI.

The three `*core` crates must stay **dependency-free** (`std` only). Most logic
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
