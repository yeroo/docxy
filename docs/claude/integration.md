# The integration plugin's host crate

## The `integration/` crate (#2317)

- The contract a plugin is written to is `docs/integration-plugins.md`: the folder it is found in,
  the ABI and its symbols, the methods and their JSON, threads, deadlines and crash markers. Read it
  before changing the ABI (`integration/src/abi.rs`), the protocol (`protocol.rs`) or the loader
  (`host.rs`), and change it with them.
- `host.rs`: `start` once per process, `global()` (a read before `start` fixes nothing),
  `shutdown()` from each host's quit hook. One worker thread makes every call. A call's deadline
  starts when it takes the worker's turn lock, so a call queued behind a slow `token` keeps its
  whole deadline. A call past its deadline stops the plugin for the run. `branding`, `sign_in` and
  `context` are asked once per app and kept; `ask_ahead` asks the first two for several apps at
  once. The crash marker `dxi-<app>-<pid>.marker` is written around loading, `dxi_abi`, `dxi_init`
  and each call. `shutdown()` removes it unless a call was abandoned.
- `accent.rs` holds the colour rule every host applies to the plugin's accents (`check` per role on
  the theme's background); `win_icon.rs` sets a window's icon (Windows).
- The hosts: the suite (`suite/docxy/src/brand.rs`, `docs/claude/suite-shell.md`); mailcore takes
  none of this crate, only a `TokenBroker` a host wraps around `token` (`docs/claude/mail.md`).
- Tests: `integration/tests/host.rs` against the fixture plugin `tests/integration_fixture`, whose
  header lists its modes (`slow`, `slow-token`, `slow-abi`, `panic`, and more); `tests/global.rs` in
  a test binary of its own (the process-wide state); `tests/load_flags.rs` (Windows: the DLL search
  flags, with `tests/dependency_fixture`).
