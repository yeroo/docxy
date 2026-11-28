# The integration plugin's host crate

## The `integration/` crate (#2317)

- The contract a plugin is written to is `docs/integration-plugins.md`: the folder it is found in,
  the ABI and its symbols, the methods and their JSON, threads, deadlines and crash markers. Read it
  before changing the ABI (`integration/src/abi.rs`), the protocol (`protocol.rs`) or the loader
  (`host.rs`), and change it with them.
- `host.rs`: `start` once per process, `global()`, `shutdown()` from each host's quit hook;
  `ask_at_start` waits for the host's own branding and asks the rest with `ask_in_background`
  (thread `integration-asks`). The waiting reads (`branding`, `sign_in`, `context`, `token`) are
  for threads other than the UI's; the UI thread reads `kept_branding`, `kept_sign_in` and
  `kept_context` (`Kept::NotYet` while a call can still answer). `app()` names the application
  the process started the plugin for: Looksy and Folksy wait and take notices only when it is
  theirs, the suite in the suite.
- `accent.rs` holds the colour rule every host applies to the plugin's accents (`check` per role on
  the theme's background); `win_icon.rs` sets a window's icon (Windows).
- The hosts: the suite (`suite/docxy/src/brand.rs`, `docs/claude/suite-shell.md`); mailcore takes
  none of this crate, only a `TokenBroker` a host wraps around `token` (`docs/claude/mail.md`; the
  `TokenBroker` doc says when the broker is asked) (#2317).
- Tests: `integration/tests/host.rs` against the fixture plugin `tests/integration_fixture`, whose
  header lists its modes; `tests/global.rs` in
  a test binary of its own (the process-wide state); `tests/load_flags.rs` (Windows: the DLL search
  flags, with `tests/dependency_fixture`).
