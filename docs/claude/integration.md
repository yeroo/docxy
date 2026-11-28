# The integration plugin's host crate

## The `integration/` crate (#2317)

- The contract a plugin is written to is `docs/integration-plugins.md`: the folder it is found in,
  the ABI and its symbols, the methods and their JSON, threads, deadlines and crash markers. Read it
  before changing the ABI (`integration/src/abi.rs`), the protocol (`protocol.rs`) or the loader
  (`host.rs`), and change it with them.
- `host.rs`: `start` once per process, `global()`, `shutdown()` from each host's quit hook;
  `ask_ahead_by` asks a list of `Ask`s within a budget that ends at a given instant, leaving the
  call the budget cuts short in flight (`Pending`, settled by the next call or read).
- `accent.rs` holds the colour rule every host applies to the plugin's accents (`check` per role on
  the theme's background); `win_icon.rs` sets a window's icon (Windows).
- The hosts: the suite (`suite/docxy/src/brand.rs`, `docs/claude/suite-shell.md`); mailcore takes
  none of this crate, only a `TokenBroker` a host wraps around `token` (`docs/claude/mail.md`; the
  `TokenBroker` doc says when the broker is asked) (#2317).
- Tests: `integration/tests/host.rs` against the fixture plugin `tests/integration_fixture`, whose
  header lists its modes; `tests/global.rs` in
  a test binary of its own (the process-wide state); `tests/load_flags.rs` (Windows: the DLL search
  flags, with `tests/dependency_fixture`).
