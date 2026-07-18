# lookxy contacts + recipient autocomplete — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-18.
**Builds on:** lookxy v1 (mailcore store + sync + Graph client, auth-code+PKCE), v2 (compose via editcore, drafts+send, calendar), v3 (thread view).

## Goal

As the user types into the compose **To / Cc / Bcc** fields, offer ranked
recipient suggestions and insert a structured `Name <addr>` on selection.
Suggestions come from a local **contacts index** populated from two sources:
the people the user already corresponds with (mined from synced mail) and,
when available, the corporate directory via Microsoft Graph `/me/people`.
This work also adds the missing **Bcc** field to compose.

## Product decisions (locked)

1. **Two sources, one index.** A store-backed `contacts` index is the single
   query surface. Local mail mining populates it with zero new scope; Graph
   `/me/people` augments it with org colleagues. Autocomplete always queries
   the local index — instant, offline, and never blocked on Graph.
2. **Graceful degradation.** If `People.Read` is denied (EPAM Conditional
   Access, unconsented, or offline), the Graph source is silently skipped and
   the feature runs on local contacts alone. It degrades; it never breaks.
3. **Bcc added now.** A Bcc compose field is threaded through the draft store
   and the Graph send pipeline (`bccRecipients`), with the same autocomplete
   as To/Cc.

## Architecture

The contacts index is a new store table populated by two engine-driven
sources (a local miner and a Graph people sync) and read by a ranked
`search_contacts` query. The compose view gains an autocomplete dropdown over
the recipient fields plus a Bcc field. Responsibilities split cleanly:

- **mailcore/store** owns the `contacts` table, `upsert_contact`, and the
  ranked `search_contacts` — the query surface.
- **mailcore** owns the local miner (`refresh_local_contacts`) — pure
  transformation of stored mail into contact rows.
- **mailcore/graph + mailcore/sync** own the `/me/people` fetch and its
  scheduling + graceful-degradation.
- **lookxy** owns the autocomplete dropdown state/keys and the Bcc field.
- **auth/store/graph** thread Bcc through send.

No per-keystroke Graph calls: `/me/people` is fetched in bulk and cached; the
keystroke path only ever hits the local SQLite index.

## Components

### 1. Contacts store — `contacts` table + queries

New table, one row per **normalized (lowercased) address**:

| column      | meaning                                                        |
|-------------|----------------------------------------------------------------|
| `address`   | PK, lowercased email address                                   |
| `name`      | best display name seen for this address                        |
| `source`    | `local` \| `graph` \| `both`                                   |
| `last_seen` | ISO-8601 of the most recent mail this address appeared in      |
| `frequency` | count of messages this address appeared in (local signal)      |
| `relevance` | Graph `/me/people` rank (0 = most relevant), nullable          |

Methods on `Store`:

- `upsert_contact(&Contact)` — merge semantics: insert or update the row for
  `address`. Local mining bumps `frequency` and advances `last_seen`, and sets
  `name` when the stored one is empty; Graph sync sets `relevance` and a
  non-empty `name`; `source` becomes `both` when both have contributed. A
  merge never regresses a non-empty `name` to empty or `frequency` downward.
- `search_contacts(query, limit) -> Vec<Contact>` — ranked match (see engine).
- `Contact { name: String, address: String, source, last_seen, frequency, relevance: Option<i64> }`.

Migration adds the table (and its index) following the store's existing
`schema_version`/migration pattern.

### 2. Local contact miner — `mailcore`

`Store::refresh_local_contacts()`: scans the `messages` table's
`from_name`/`from_addr` and the
`to_recipients`/`cc_recipients` encoded strings (reusing the existing
recipient parser that splits `Name <addr>; ...`), and `upsert_contact`s each
`(name, address)` with `last_seen` = the message's date and a `frequency`
increment. Malformed or empty addresses are skipped. It is a full,
idempotent rebuild-from-mail pass (mailbox-scale, bounded), safe to run
repeatedly.

The **sync engine runs it once per sync pass** (after the per-folder deltas
complete), so contacts stay fresh as mail arrives, decoupled from the
message-write path.

### 3. Graph people sync — `mailcore/graph` + `mailcore/sync`

- `GraphClient::people() -> Result<Vec<Person>>` calls `GET /me/people`
  (relevance-ranked), requesting the top ~200. `Person { name, address, rank }`
  (rank = position in the relevance-ordered response). Parses the same
  hand-rolled-JSON way the rest of the client does; entries without an email
  address are skipped.
- The **sync engine calls `people()` once per sign-in and periodically**
  (e.g. at most once per configured refresh window), and `upsert_contact`s
  each person with `source=graph`, `relevance=rank`.
- **Graceful degradation:** a `People.Read`-insufficient `403`, a Conditional-
  Access failure, an offline error, or any other people-fetch failure is
  caught and does **not** fail the sync pass or surface an error modal. It
  sets a one-time soft notice ("directory suggestions unavailable — using
  local contacts; re-sign-in to enable") and leaves the local contacts intact.

### 4. Autocomplete engine — `search_contacts` ranking

`search_contacts(query, limit)` (query is the lowercased current token):

- **Match:** `name` LIKE `%query%` OR `address` LIKE `%query%` (case-insensitive).
- **Order:** prefix matches (name or address starts with `query`) before
  interior matches; then by a descending score combining `frequency`,
  recency (`last_seen`), and Graph `relevance` (more-relevant/more-frequent/
  more-recent first); ties broken by `name`.
- **Limit:** `limit` (UI passes ~8). Deduped by address (the PK guarantees it).

### 5. Compose UI — Bcc field + autocomplete dropdown — `lookxy`

**Bcc field:** `Compose` gains `bcc: String`; `ComposeField` gains `Bcc`; the
focus cycle becomes To → Cc → **Bcc** → Subject → Body; a Bcc row is drawn
between Cc and Subject.

**Autocomplete dropdown:** a new `Compose` sub-state
`autocomplete: Option<Autocomplete>` where
`Autocomplete { field: ComposeField, query: String, matches: Vec<Contact>, index: usize }`.

- On a keystroke/backspace in a recipient field (To/Cc/Bcc), recompute the
  **current token** — the text after the last `;` or `,` in that field,
  trimmed — and if non-empty, run `search_contacts(token, 8)` and open/refresh
  the dropdown; if empty, close it.
- Keys **while the dropdown is open:** `↓`/`↑` move `index` (clamped);
  `Enter` or `Tab` accepts the highlighted match — replacing the current token
  in the field with `Name <addr>; ` (ready for the next recipient) and closing
  the dropdown; `Esc` closes it without accepting; any character/backspace
  edits the field and refilters.
- Keys **while it is closed:** `Tab` cycles focus (today's behavior); other
  keys type as today.
- The dropdown renders as a small overlay directly below the focused field,
  highlighting `index`, bounded to the match count.

### 6. Bcc through the draft + send pipeline

- **Store:** add a `bcc_recipients` column to the `messages` table (where
  drafts live, keyed by `is_draft`) via a migration; `update_draft_fields`
  (and the draft read) include bcc. Bcc is
  encoded/parsed with the same `encode_recipients`/`parse_recipients` path as
  To/Cc.
- **Graph:** the message JSON built for save-draft/send includes
  `bccRecipients` alongside `toRecipients`/`ccRecipients`.
- **Compose load/save:** reply/forward/new and draft-resume read and write bcc
  like to/cc.

### 7. Auth scope

The auth scope constant changes from `Mail.ReadWrite offline_access` to
`Mail.ReadWrite People.Read offline_access`. Since the user's first real
sign-in has not yet happened, that sign-in requests all scopes together (no
separate re-consent). A pre-existing Mail-only cached token simply yields a
`403` from `people()`, which the graceful-degradation path handles (local
contacts + the re-sign-in notice).

## Data flow

```
type in To/Cc/Bcc
  → extract current token (after last ; or ,)
  → store.search_contacts(token, 8)
  → dropdown (↓/↑ move, Enter/Tab accept, Esc close)
  → accept → field := "…prefix… Name <addr>; "

contacts populated by the sync engine each pass:
  refresh_local_contacts()   (always)
  people() → upsert_contact  (when People.Read granted; else skipped, notice set)
```

## Error handling & edge cases

- **People.Read denied / CA-blocked / offline** → Graph source skipped, no
  crash, no error modal; one-time soft notice; local contacts unaffected.
- **Empty contacts index** → no dropdown ever opens; typing works normally.
- **Malformed/blank addresses in mail** → skipped by the miner.
- **Token with no matches** → dropdown closed (nothing to show).
- **Accepting into a multi-recipient field** → only the current (last) token is
  replaced; already-entered recipients before the last `;`/`,` are untouched.
- **Duplicate suggestion already typed** → allowed (the parser/dedup on send is
  the backstop); not specially filtered in v1.
- **Bcc empty** → omitted from the sent message (no empty `bccRecipients`).

## Testing

**mailcore (unit):**
- `upsert_contact` merge: local then graph on the same address → `source=both`,
  name/relevance/frequency/last_seen resolved per the merge rules; no
  regression of a non-empty name or frequency.
- `search_contacts` ranking: prefix-before-interior; frequency/recency/
  relevance ordering; case-insensitive; `limit` respected; address dedup.
- `refresh_local_contacts`: seeded messages with from/to/cc → expected contact
  rows with correct frequency/last_seen; malformed addresses skipped.
- `people()`: parses a `/me/people` JSON fixture into ranked `Person`s;
  address-less entries skipped; a `403` body surfaces as the degradation path
  (engine test), not an error.

**lookxy (unit + render):**
- Bcc field: focus cycle includes Bcc; draw shows a Bcc row; bcc typed/edited.
- Token extraction: after `a@x; bo` the token is `bo`; after `a@x;` it is empty.
- Dropdown: opens on a matching token, `↓`/`↑` move within bounds, `Enter`/`Tab`
  accept and rewrite the field to `…; Name <addr>; `, `Esc` closes, closed-Tab
  still cycles focus.
- Send: bcc reaches `update_draft_fields` and the message JSON's
  `bccRecipients`.

## Scope boundaries (YAGNI)

- **No full `/users` directory search** — needs admin-consented scopes EPAM CA
  withholds; `/me/people` is the reachable delegated source.
- **No per-keystroke Graph calls** — `/me/people` is cached; keystrokes hit
  only the local index.
- **No contact-management UI** — the index is read-only; no add/edit/delete.
- **No structured-recipient refactor** — autocomplete inserts `Name <addr>`
  text reusing the existing recipient parser; the earlier free-text parsing
  bug (H1) is already fixed, so end-to-end structured recipients are out of
  scope here.
