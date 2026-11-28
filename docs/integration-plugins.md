# Integration plugins

An integration plugin is one shared library that an organisation installs beside a host built on
docxy's crates. It can give the host:

- its product names and icons;
- its Microsoft 365 sign-in settings;
- a silent token source;
- the person's organization and desktop.

The crate is
`docxy-integration` (`integration/`). A plugin written in Rust depends on it with
`default-features = false` for the types in `abi` and `protocol`; the `host` feature is the host
side. `integration/include/docxy_integration.h` declares the same ABI for C.

## Where the host finds the plugin

- **Windows:** the `REG_SZ` value `PluginDir` under `HKEY_LOCAL_MACHINE\SOFTWARE\docxy\Integration`,
  read from the 64-bit view, and nothing else: a `REG_EXPAND_SZ` value names no folder. Only an
  administrator can set it.
- **Elsewhere:** the environment variable `DOCXY_INTEGRATION_DIR`.

The folder must be an absolute path; the host refuses a relative one and says why. The folder and
its files must be writable by administrators only: the host loads what is there, and the libraries
the plugin needs are looked for in that folder first.

There is no per-user default folder, as format plugins have: this plugin gets tokens and the person's
identity. With no value, or an empty one, the host loads nothing, opens no file and shows nothing.

The folder must hold exactly one library whose name starts with `dxi_` (any case) and ends with the
platform's library extension (`.dll`, `.so`, `.dylib`). A Unix `lib` prefix does not count. With no
such library, or several, the host loads none and says why.

On Windows the library is loaded with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, so the libraries it needs are found in its own folder and the
default folders (the host exe's folder, the system's, and any folder the host adds with
`AddDllDirectory`), never in the current directory or on `PATH`. A library is never unloaded.

## The symbols

```c
typedef struct { uint8_t* ptr; size_t len; } DxiBuf;

uint32_t dxi_abi(void);
int32_t  dxi_init(const uint8_t* host_json, size_t len, DxiBuf* out);
int32_t  dxi_call(const char* method, const uint8_t* req_json, size_t len, DxiBuf* out);
void     dxi_free(DxiBuf* buf);
int32_t  dxi_last_error(DxiBuf* utf8_out);
```

All five are required. The host calls `dxi_abi` before it looks up any other symbol, then
`dxi_init` once, then `dxi_call` for each method.

- `dxi_init` gets `{"abi":1,"app":"suite|mail|chat","app_version":"x.y.z","os":"windows"}`. `app` is
  the application the process runs first. The answer is `{"plugin":"<name>","version":"<semver>",
  "methods":["branding","sign_in","token","context"]}`. The host never calls a method that the list
  leaves out. A non-zero return refuses the host, and `dxi_last_error` says why.
- `dxi_call` gets the method's name, NUL-terminated, and its request.
- `dxi_last_error` fills `utf8_out` with the message for the last non-zero return and returns 0. It
  returns non-zero when it has no message.

Return codes: 0 ok, 1 unknown method, 2 malformed request, 3 not configured, 4 internal error.

## Versions

A host accepts the interface versions from its `MIN_ABI_VERSION` to its `ABI_VERSION` (`abi.rs`).
It refuses a library outside that range, a library without one of the five
symbols, or one whose `dxi_init` refuses, and calls it no further. The message names the file and
the reason. A later host can add version 2 and keep loading version-1 plugins until it raises its
minimum.

## Buffers and strings

The plugin allocates every `DxiBuf` it fills. The host passes `{NULL, 0}`, copies the bytes, then
calls `dxi_free` once for each buffer whose `ptr` is not NULL, whatever the return code. What the
host passes in is valid only for the duration of the call. All text is UTF-8 JSON; no other type
crosses the boundary.

## Threads, deadlines, panics and crashes

- One worker thread per process makes every call, one at a time, and never the host's UI thread.
  The library is loaded on that thread too.
- Each wait has a deadline: 5 s for finding the library in its folder, loading it and `dxi_init`,
  5 s for `branding`, `sign_in` and `context`, 60 s for `token`. A call's deadline starts when the
  worker takes it, not while it waits behind another caller's call. A call past its deadline is abandoned (its thread stays blocked),
  and the host does not use the plugin again in that run and says so once.
- Before its first window a host waits for loading, `dxi_init` and its own application's
  `branding`, each within its own deadline. It asks the other values it uses on a thread of its
  own, right after: every other `branding` first, then each `sign_in`, then `context`. A plugin's
  first `sign_in` may be slow, so it comes after the brandings.
- The UI thread never waits on the plugin: it reads the values kept so far, and a value not yet
  answered shows the host's own until a later draw or open. A read that needs the answer (sign-in
  settings before an engine signs in, a `token`) is made on another thread, which waits for the
  call in flight.
- A host says a stop (a call past its deadline) where it says the plugin's refusal at start, on
  whichever thread the call was made. The notice stays until a status or notice line has shown it
  (`Integration::notice` reads it, `take_notice` takes it).
- A plugin must catch every panic or exception at each export and return an error code. A Rust panic
  that unwinds out of an `extern "C"` function aborts the process.
- A crash inside the plugin ends the host. Before loading, before `dxi_abi`, `dxi_init` and each
  call, the host writes a marker file `dxi-<app>-<pid>.marker`, holding the step's name, in the
  application's own config folder. It removes the file after the step, and at a normal quit unless a
  call was abandoned (a call still in flight then leaves none; no call is made after the quit). A
  host that starts and finds a marker of its application whose process no longer runs skips the
  plugin for that run, says why, and deletes the marker. A run that crashed, or ended during an
  abandoned call, leaves its marker. Markers of a process that still runs are left alone, so several
  applications and processes can run at the same time.

## Methods

Every field of an answer is optional. A field that is missing, null, of another type or empty keeps
the host's own value.

| Method | Request | Answer |
|---|---|---|
| `branding` | `{"app":"<app>"}` | `{"product","short","about_line","icon_ico_b64","icon_png_b64","accents"}`: the names in window titles and About, an `.ico` file and a PNG, each base64 (RFC 4648, with padding), and the accent colours ([Accents](#accents)) |
| `sign_in` | `{"app":"<app>"}` | `{"client_id","tenant","login_hint","cloud"}` |
| `token` | `{"app","scopes","client_id","tenant"}`, `scopes` space-separated | `{"status":"ok","access_token","expires_at_unix","account","scopes"}`, `{"status":"interaction_required","why"}` or `{"status":"unavailable","why"}` |
| `context` | `{}` | `{"organization":{"id","name"},"desktop":{"id","name"}}` |

### Accents

`accents` is `{"light":{"primary","link","highlight"},"dark":{...}}`, each colour `#RRGGBB` (`#` and
six hex digits, in either case). A role that is missing or written otherwise is absent, and the
host keeps its own colour for it.

| Role | What the host draws with it | The host takes it when (WCAG 2.x contrast) |
|---|---|---|
| `primary` | primary buttons, the selection, the focus ring | always; it draws black or white text on it, whichever has the higher contrast (one of them always reaches 4.5:1) |
| `link` | links and the secondary accent | it reaches 4.5:1 on the theme's background |
| `highlight` | small marks only (the active tab's, an unread mark), never a large fill | it reaches 4.5:1 on the theme's background |

The host draws the set of its current theme and follows a theme switch. For a colour it does not
take it keeps its own and says so once in its log. `integration/src/accent.rs` holds the rule, for
every host. Window and background colours do not change.

The host asks `branding` and `sign_in` once per application and `context` once per process, and
keeps the answers. It asks `token` each time it needs a token. An `ok` answer without
`access_token` or `expires_at_unix`, or a status the host does not know, counts as `unavailable`.

## What the host shows and keeps

- **Status:** not configured, loaded (the file and the plugin's version), or refused (the file or
  folder, and why). A plugin that stopped answering is refused from then on.
- **The notice:** the message about a refusal or a stop, given once, for the host to show.
- **Not to be logged or shown:** the access token and the login hint.
