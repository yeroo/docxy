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

The workspace is two crates:

- **`docxcore`** — pure, `std`-only OOXML I/O (ZIP/DEFLATE/XML, the document
  model, rendering, and the PDF writer). It must stay **dependency-free**.
- **`docxy`** — the terminal UI (ratatui), clipboard, and image rendering.

Most logic lives in `docxcore` and is covered by fast, pure unit tests — please
add tests there for behavior changes.

## Guidelines

- Format with `cargo fmt` (rustfmt defaults); keep `clippy` clean.
- Keep **`docxcore` dependency-free** — runtime crates belong only in `docxy`.
- Keep changes focused; one logical change per pull request.
- If you change behavior, describe it in the PR (and update the README if it is
  user-facing).

## Reporting issues

Open an issue with the exact command you ran and, if possible, a minimal `.docx`
that reproduces the problem. For image-rendering issues, include your terminal.
