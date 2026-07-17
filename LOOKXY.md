# lookxy — Outlook/Exchange mail in the terminal

`lookxy` is the mail-client sibling of `docxy`/`xlsxy`/`yppxy`: where those sit
on `docxcore`/`gridcore`/`projcore`, this one sits on a headless engine,
`mailcore`, and adds a terminal (TUI) shell over it — a folder tree, a message
list, and a reading pane, kept live by a background sync thread talking to
Microsoft Graph. For the document/spreadsheet/project sides see the
[README](README.md), [SPREADSHEET.md](SPREADSHEET.md), and
[PROJECT.md](PROJECT.md).

## Why this exists

Reading and triaging mail from a terminal — over SSH, in tmux, without
switching to a GUI — is the same pitch as the rest of this repo: a fast,
keyboard-driven, dependency-lean tool instead of a heavyweight client. Auth,
sync, and local storage live in `mailcore` (no UI dependency, fully
fake-server-testable); `lookxy` is the `ratatui` shell that renders it and
routes keystrokes.

## Sign-in (first run)

There's no username/password prompt and no device code. `lookxy` signs in
with the standard OAuth2 **authorization-code + PKCE** flow:

1. Launch `lookxy`. With no cached token, it shows a sign-in prompt.
2. Press **Enter**. `lookxy` opens your system browser to a Microsoft
   `login.microsoftonline.com` authorize page (a `http://localhost:<port>`
   loopback redirect handles the callback — no secret is ever typed into
   `lookxy` itself). If the browser doesn't open automatically, the modal
   also shows the URL to copy in by hand.
3. Sign in with your Microsoft 365 / Exchange Online account in the browser
   and approve the requested permissions.
4. The browser redirects back to the loopback listener; `lookxy` exchanges
   the code for tokens, caches them (DPAPI-encrypted — see
   [Security & privacy](#security--privacy) below), and starts syncing.

Sign-in only happens once per machine per account; after that, `lookxy`
refreshes the access token silently in the background using the cached
refresh token.

## Usage & keybindings

```sh
lookxy
```

The screen is three panes — **Folders** | **Message list** | **Reading
pane** — plus a status bar. `Tab` cycles focus between them.

| Keys | Action |
|------|--------|
| `Tab` | cycle focus: Folders → List → Reading → Folders |
| `↑`/`↓`, `j`/`k` | move the selection in the focused pane |
| `Enter` | activate: pick a folder, or open the highlighted message |
| `m` / `u` | mark the highlighted message read / unread |
| `f` | toggle the flag on the highlighted message |
| `d` / `Delete` | delete the highlighted message |
| `v` | move the highlighted message — opens a folder picker (`↑`/`↓`/`j`/`k` to choose, `Enter` to confirm, `Esc` to cancel) |
| `a` | open the attachments popup for the highlighted message (`↑`/`↓`/`j`/`k` to pick, `Enter` to save to Downloads, `o` to save-and-open, `Esc` to close) |
| `/` | open the search prompt — type a query, `Enter` to run it against the local full-text index, `↑`/`↓` to move through results, `Esc` to return to the folder view |
| `q`, `Ctrl-C` | quit |

Every triage action (`m`/`u`/`f`/`v`/`d`) writes to the local store
immediately (so the list updates without waiting on the network) and queues
the matching change to push to Exchange in the background; a failed push is
retried automatically and never silently drops your action. The background
sync engine also re-syncs on its own on a timer (`refresh_secs`, see
[Configuration](#configuration)), so folders and messages refresh even if
you never touch anything.

## Storage locations

Everything lives under your Windows user profile, like Outlook's own OST:

- **Mail database**: `%LOCALAPPDATA%\lookxy\<account>\mail.db` — a SQLite
  database per signed-in account (the account's UPN, sanitized into a single
  path component, e.g. `me@contoso.com` → `me_contoso.com`). Folders,
  messages, bodies, attachment metadata, the full-text search index, and the
  outbox of pending Graph operations all live here.
- **Token cache**: `%LOCALAPPDATA%\lookxy\token.bin` — the OAuth access and
  refresh tokens, **encrypted at rest with Windows DPAPI** (`CryptProtectData`,
  scoped to your Windows user account). See below for what that does and
  doesn't protect against.
- **Config file** (optional): `%APPDATA%\lookxy\config.json` — see
  [Configuration](#configuration).

Deleting the whole `%LOCALAPPDATA%\lookxy` folder resets `lookxy` back to a
clean first run (a fresh sign-in and a fresh local sync from Exchange).

## Configuration

`lookxy` runs with no configuration at all — every setting has a built-in
default. If you want to override one, create
`%APPDATA%\lookxy\config.json`:

```json
{
  "client_id": "14d82eec-204b-4c2f-b7e8-296a70dab67e",
  "backfill_days": 180,
  "refresh_secs": 60
}
```

| Field | Default | Meaning |
|-------|---------|---------|
| `client_id` | Microsoft Graph CLI's client id | The Entra ID app registration `lookxy` authenticates as. |
| `backfill_days` | `180` | How many days of mail history the sync engine backfills on first run. |
| `refresh_secs` | `60` | The background sync engine's poll interval (seconds): how often it re-checks Exchange for changes on its own, with no user action needed. Raising or lowering it takes effect immediately at the next launch — it sets the real interval, not just a floor. |

The file is entirely optional — a missing or unparsable file (or an unknown
key in it) is silently ignored, and `lookxy` falls back to the built-in
defaults rather than refusing to start. `backfill_days` and `refresh_secs`
are also range-checked: a non-positive `refresh_secs` (which would otherwise
busy-loop, or silently disable refresh entirely if cast unchecked from a
negative number) or a `backfill_days` less than `1` (a zero/negative backfill
window is meaningless) is rejected the same way an unparsable value is — the
next-lower-precedence value (file, then default) is kept instead.

Every field can also be overridden by an environment variable, which wins
over both the file and the defaults:

| Variable | Overrides |
|----------|-----------|
| `LOOKXY_CLIENT_ID` | `client_id` |
| `LOOKXY_BACKFILL_DAYS` | `backfill_days` |
| `LOOKXY_REFRESH_SECS` | `refresh_secs` |

Precedence, highest wins: **environment variable** > **config file** >
**built-in default**.

## Security & privacy model

- **Tokens are encrypted at rest.** `token.bin` is protected with Windows
  DPAPI (`CryptProtectData`/`CryptUnprotectData`), which ties decryption to
  your Windows user account — another local account (or a copy of the file
  moved to another machine) can't decrypt it. DPAPI does **not** protect
  against another process running *as you* on the same logged-in session;
  that's the same trust boundary Windows itself, and Outlook's own credential
  storage, operate under.
- **The mail store is plaintext SQLite**, not encrypted at rest — the same
  model Outlook uses for its local `.ost` cache. Anyone with filesystem
  access to your Windows user profile (or a backup of it) can read cached
  mail content. At-rest encryption of the mail store is a possible v2
  hardening, not implemented today.
- **Secrets are never logged.** Access/refresh tokens and the PKCE code
  verifier are excluded from every log line and error message by
  construction (see `mailcore::auth`'s and `mailcore::tokencache`'s doc
  comments).
- **The default app registration's scopes are broad.** Out of the box,
  `lookxy` authenticates as the Microsoft Graph CLI's published client id — a
  Microsoft-owned, preauthorized public client that made the auth-code + PKCE
  flow work immediately without registering anything, but whose consent
  screen requests the same `Mail.ReadWrite`/`offline_access` scope any
  Graph CLI user already trusts. **The hardening path** is to register your
  own Entra ID app (a public client, `Mail.ReadWrite` + `offline_access`
  delegated scopes, a `http://localhost` redirect URI) and point `lookxy` at
  its client id via `client_id` in `config.json` or `LOOKXY_CLIENT_ID` — so
  your organization's admin consent, conditional access, and audit logs are
  scoped to an app you control rather than a shared public client.
- **Device-code sign-in doesn't work here** against tenants that enforce
  Conditional Access blocking it (this is why `lookxy` uses the browser
  auth-code + PKCE flow instead of the simpler device-code flow).

## Architecture at a glance

```
mailcore/   headless engine: OAuth2 auth-code+PKCE, a Graph REST client,
            SQLite storage (+ FTS5 full-text search), and a background sync
            thread (folders, per-folder delta sync, an outbox of pending
            mutations, retry/back-off, token refresh)
lookxy/     the TUI (ratatui): three-pane layout, keyboard routing, the
            sign-in/move/attachments/search popups, and Config
```

The sync engine is a single `std::thread` — no async runtime — driven over
`std::sync::mpsc` channels (`SyncCommand` down, `SyncEvent` up); the UI never
blocks on the network, it just reads whatever the engine has already written
to the store and reacts to events as they arrive.

## First run / verification

Since a real sign-in needs an interactive browser and a real Exchange Online
mailbox, it isn't something an automated build/test run can do. To verify a
build end-to-end:

1. `cargo build --release -p lookxy`, then run `target/release/lookxy.exe`.
2. Complete the browser sign-in (see [Sign-in](#sign-in-first-run) above)
   against your own Microsoft 365 / Exchange Online account.
3. Confirm: folders populate in the left pane; selecting one lists its
   messages; opening a message renders its body in the reading pane.
4. Exercise triage round-trips against the real mailbox: `m`/`u` mark
   read/unread, `f` flags, `d` deletes, `v` moves to another folder — then
   check Outlook (web or desktop) shows the same change.
5. `/` search for a word you know is in a message's subject or body and
   confirm it's found.
6. `a` on a message with an attachment, save it, and confirm the file lands
   in your Downloads folder with the right content.

This manual pass is deferred to the user running against a real account —
see the project report for this task for exactly what's been verified by
the automated test suite instead (fake-server integration tests covering
the same flows with no network or secrets involved).
