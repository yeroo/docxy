# lookxy Contacts + Recipient Autocomplete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** As-you-type To/Cc/Bcc recipient autocomplete in compose, from a store-backed contacts index populated by local mail mining and Microsoft Graph `/me/people`, plus a new Bcc field end-to-end.

**Architecture:** A new `contacts` table is the single query surface (`search_contacts`), populated by two engine-driven sources — a local miner (`refresh_local_contacts`) and a `/me/people` sync (graceful-degrading if `People.Read` is denied). The compose view gains a Bcc field (threaded through the draft store + Graph send) and an autocomplete dropdown over the recipient fields that queries only the local index.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), rusqlite (bundled SQLite + FTS5), ratatui 0.29, hand-rolled `mailcore::json` (no serde), ureq+rustls Graph client. No new dependencies.

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **No new dependencies.** Hand-rolled `mailcore::json` for all JSON.
- **MSRV 1.88, edition 2024.** Let-chains available; extern blocks are `unsafe extern`.
- **CI runs `cargo clippy --all-targets -- -D warnings` on ubuntu/macos/windows.** No warnings; no `#[cfg(windows)]`-only bindings left unused on Unix. Run `bash "$LCARGO" fmt` before every commit.
- **Preserve existing behavior.** Existing flat/threaded mail, existing compose To/Cc/Subject/Body, and existing draft save/send must stay green; do not change their behavior except where a task explicitly extends it (Bcc, autocomplete).
- **Graph is best-effort.** A `/me/people` failure (403 insufficient scope, Conditional-Access, offline, any error) must NEVER fail a sync pass or raise an error modal — the local contacts carry the feature.
- **Recipient encoding is fixed:** drafts store recipients as the flat `Name <addr>; Name <addr>` text that `store::encode_recipients` writes and `sync::outbox::parse_recipients` reads. Reuse those, do not invent a second format.
- **`MessageRow` column order is fixed** (id, folder_id, conversation_id, subject, from_name, from_addr, to_recipients, cc_recipients, received_at, sent_at, is_read, is_flagged, has_attachments, importance, preview, is_draft). Task 5 appends `bcc_recipients` as the **last** column (index 16) to avoid reshuffling the existing indices.

---

### Task 1: Contacts table + store queries

**Files:**
- Modify: `mailcore/src/store/schema.rs` (add the `contacts` table to `SCHEMA_SQL`)
- Modify: `mailcore/src/store/mod.rs` (add `Contact`, `upsert_contact`, `search_contacts`; add a test)

**Interfaces:**
- Produces:
  - `pub struct Contact { pub name: String, pub address: String, pub source: String, pub last_seen: String, pub frequency: i64, pub relevance: Option<i64> }`
  - `pub fn upsert_contact(&self, c: &Contact) -> Result<(), StoreError>` — merge by `address`.
  - `pub fn search_contacts(&self, query: &str, limit: i64) -> Result<Vec<Contact>, StoreError>` — `query` is matched case-insensitively; caller may pass any case (the method lowercases internally).

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/store/mod.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn upsert_contact_merges_local_then_graph() {
        let s = Store::open_in_memory().unwrap();
        // local mining: a name + frequency + recency, no relevance
        s.upsert_contact(&Contact {
            name: "Bob Jones".into(), address: "bob@x.com".into(), source: "local".into(),
            last_seen: "2026-07-10T00:00:00Z".into(), frequency: 3, relevance: None,
        }).unwrap();
        // graph sync: same address, a display name + relevance, no local signal
        s.upsert_contact(&Contact {
            name: "Robert Jones".into(), address: "bob@x.com".into(), source: "graph".into(),
            last_seen: "".into(), frequency: 0, relevance: Some(2),
        }).unwrap();

        let got = s.search_contacts("bob", 10).unwrap();
        assert_eq!(got.len(), 1);
        let c = &got[0];
        assert_eq!(c.source, "both");                 // both sources contributed
        assert_eq!(c.relevance, Some(2));             // graph relevance kept
        assert_eq!(c.frequency, 3);                   // local frequency kept (MAX, not clobbered to 0)
        assert_eq!(c.last_seen, "2026-07-10T00:00:00Z"); // recency kept (MAX, not clobbered to "")
        assert_eq!(c.name, "Robert Jones");           // non-empty graph name applied
    }

    #[test]
    fn search_contacts_ranks_prefix_first_then_frequency() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_contact(&Contact { name: "Ann Lee".into(), address: "ann@x.com".into(), source: "local".into(), last_seen: "2026-07-01T00:00:00Z".into(), frequency: 1, relevance: None }).unwrap();
        s.upsert_contact(&Contact { name: "Danny".into(), address: "dan@x.com".into(), source: "local".into(), last_seen: "2026-07-02T00:00:00Z".into(), frequency: 9, relevance: None }).unwrap();
        // "an": "Ann"/"ann@" are PREFIX matches; "Danny"/"dan@" contain "an" only interior.
        let got = s.search_contacts("an", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["ann@x.com", "dan@x.com"]); // prefix match ranks ahead of higher-frequency interior match
        // limit is respected
        assert_eq!(s.search_contacts("an", 1).unwrap().len(), 1);
        // matching is case-insensitive on both name and address
        assert_eq!(s.search_contacts("ANN", 10).unwrap()[0].address, "ann@x.com");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore upsert_contact search_contacts`
Expected: FAIL — `cannot find type Contact` / `no method named upsert_contact`.

- [ ] **Step 3: Implement**

In `mailcore/src/store/schema.rs`, add this table to the `SCHEMA_SQL` string (after the `messages` table, before `meta` — anywhere in the `CREATE TABLE IF NOT EXISTS` block is fine):

```sql
CREATE TABLE IF NOT EXISTS contacts (
    address    TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT 'local',
    last_seen  TEXT NOT NULL DEFAULT '',
    frequency  INTEGER NOT NULL DEFAULT 0,
    relevance  INTEGER
);
```

In `mailcore/src/store/mod.rs`, add the struct near `MessageRow` (after it):

```rust
/// A `contacts` row — one per normalized (lowercased) email address, the
/// autocomplete query surface. `source` is `local`/`graph`/`both`;
/// `relevance` is the Graph `/me/people` rank (lower = more relevant),
/// `None` for a purely locally-mined contact.
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub name: String,
    pub address: String,
    pub source: String,
    pub last_seen: String,
    pub frequency: i64,
    pub relevance: Option<i64>,
}
```

Add the methods to `impl Store`:

```rust
    /// Inserts or merges a contact keyed by `address`. Merge rules keep the
    /// strongest signal from either source: a non-empty `name` wins over an
    /// empty one; `source` becomes `both` once both a local and a graph upsert
    /// have touched the row; `last_seen`/`frequency` take the MAX (so re-running
    /// the local miner is idempotent, and a graph upsert with 0/"" never lowers
    /// them); `relevance` takes the incoming value when present, else keeps the
    /// stored one.
    pub fn upsert_contact(&self, c: &Contact) -> Result<(), StoreError> {
        let address = c.address.to_lowercase();
        self.conn.execute(
            "INSERT INTO contacts (address, name, source, last_seen, frequency, relevance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(address) DO UPDATE SET
                 name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE contacts.name END,
                 source = CASE WHEN contacts.source = excluded.source THEN contacts.source ELSE 'both' END,
                 last_seen = MAX(contacts.last_seen, excluded.last_seen),
                 frequency = MAX(contacts.frequency, excluded.frequency),
                 relevance = COALESCE(excluded.relevance, contacts.relevance)",
            params![address, c.name, c.source, c.last_seen, c.frequency, c.relevance],
        )?;
        Ok(())
    }

    /// Ranked autocomplete matches for `query` (matched case-insensitively
    /// against name and address). Prefix matches rank ahead of interior
    /// matches; then by Graph relevance (lower first, nulls last), then
    /// frequency (higher first), then recency, then name.
    pub fn search_contacts(&self, query: &str, limit: i64) -> Result<Vec<Contact>, StoreError> {
        let q = query.to_lowercase();
        let mut stmt = self.conn.prepare(
            "SELECT address, name, source, last_seen, frequency, relevance
             FROM contacts
             WHERE lower(name) LIKE '%' || ?1 || '%' OR lower(address) LIKE '%' || ?1 || '%'
             ORDER BY
                 (CASE WHEN lower(name) LIKE ?1 || '%' OR lower(address) LIKE ?1 || '%' THEN 0 ELSE 1 END) ASC,
                 (CASE WHEN relevance IS NULL THEN 1 ELSE 0 END) ASC,
                 relevance ASC,
                 frequency DESC,
                 last_seen DESC,
                 name ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q, limit], |row| {
                Ok(Contact {
                    address: row.get(0)?,
                    name: row.get(1)?,
                    source: row.get(2)?,
                    last_seen: row.get(3)?,
                    frequency: row.get(4)?,
                    relevance: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore upsert_contact search_contacts`
Expected: PASS (2 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/store/schema.rs mailcore/src/store/mod.rs
git commit -m "mailcore: contacts table + upsert_contact/search_contacts"
```

---

### Task 2: Local contact miner

**Files:**
- Modify: `mailcore/src/sync/outbox.rs` (make `parse_recipients` `pub(crate)`)
- Modify: `mailcore/src/store/mod.rs` (add `Store::refresh_local_contacts`; add a test)

**Interfaces:**
- Consumes: `Contact`/`upsert_contact` (Task 1); `crate::sync::outbox::parse_recipients` (made `pub(crate)` here).
- Produces: `pub fn refresh_local_contacts(&self) -> Result<(), StoreError>` — idempotent full rebuild of the local contact signal from the `messages` table.

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/store/mod.rs` tests (reuse the module's existing `Message`/`Recipient` seeding — see `upserts_and_lists_messages_newest_first`):

```rust
    #[test]
    fn refresh_local_contacts_mines_from_and_to_and_cc() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder { id: "inbox".into(), display_name: "Inbox".into(), parent_id: None, total_count: 0, unread_count: 0, well_known_name: Some("inbox".into()) }).unwrap();
        // A message FROM Alice, TO Bob + Carol.
        let mut m = msg("1", false); // helper sets received "2026-07-01T00:00:00Z"
        m.from = Recipient { name: "Alice".into(), address: "alice@x.com".into() };
        m.to = vec![Recipient { name: "Bob".into(), address: "bob@x.com".into() }];
        m.cc = vec![Recipient { name: "Carol".into(), address: "carol@x.com".into() }];
        s.upsert_message("inbox", &m).unwrap();

        s.refresh_local_contacts().unwrap();
        // All three parties are mined as contacts.
        let names: Vec<String> = ["alice", "bob", "carol"].iter()
            .map(|q| s.search_contacts(q, 1).unwrap().into_iter().next().map(|c| c.address).unwrap_or_default())
            .collect();
        assert_eq!(names, ["alice@x.com", "bob@x.com", "carol@x.com"]);
        // Idempotent: a second pass does not double-count frequency.
        let f1 = s.search_contacts("alice", 1).unwrap()[0].frequency;
        s.refresh_local_contacts().unwrap();
        let f2 = s.search_contacts("alice", 1).unwrap()[0].frequency;
        assert_eq!(f1, f2);
        assert_eq!(f1, 1); // appeared in exactly one message
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore refresh_local_contacts`
Expected: FAIL — `no method named refresh_local_contacts`.

- [ ] **Step 3: Implement**

In `mailcore/src/sync/outbox.rs`, change `fn parse_recipients` to `pub(crate) fn parse_recipients` (visibility only — no body change).

In `mailcore/src/store/mod.rs`, add to `impl Store`:

```rust
    /// Rebuilds the local contact signal from the `messages` table: every
    /// distinct address seen as a sender or a to/cc recipient becomes a
    /// contact, with `frequency` = how many messages it appeared in and
    /// `last_seen` = the most recent of those messages' dates. Idempotent —
    /// safe to run every sync pass (the miner computes exact counts and
    /// `upsert_contact` takes the MAX, so re-runs don't inflate anything).
    pub fn refresh_local_contacts(&self) -> Result<(), StoreError> {
        use std::collections::HashMap;
        // (name, last_seen, count) aggregated by lowercased address.
        let mut agg: HashMap<String, (String, String, i64)> = HashMap::new();
        let mut consider = |name: &str, addr: &str, date: &str, agg: &mut HashMap<String, (String, String, i64)>| {
            let addr = addr.trim().to_lowercase();
            if addr.is_empty() || !addr.contains('@') {
                return;
            }
            let e = agg.entry(addr).or_insert_with(|| (String::new(), String::new(), 0));
            if e.0.is_empty() && !name.trim().is_empty() {
                e.0 = name.trim().to_string();
            }
            if date > e.1.as_str() {
                e.1 = date.to_string();
            }
            e.2 += 1;
        };

        let mut stmt = self.conn.prepare(
            "SELECT from_name, from_addr, to_recipients, cc_recipients, received_at, sent_at FROM messages",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (from_name, from_addr, to, cc, received, sent) in rows {
            let date = if !received.is_empty() { received.as_str() } else { sent.as_str() };
            consider(&from_name, &from_addr, date, &mut agg);
            for r in crate::sync::outbox::parse_recipients(&to) {
                consider(&r.name, &r.address, date, &mut agg);
            }
            for r in crate::sync::outbox::parse_recipients(&cc) {
                consider(&r.name, &r.address, date, &mut agg);
            }
        }

        for (address, (name, last_seen, frequency)) in agg {
            self.upsert_contact(&Contact {
                name,
                address,
                source: "local".to_string(),
                last_seen,
                frequency,
                relevance: None,
            })?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore refresh_local_contacts`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/sync/outbox.rs mailcore/src/store/mod.rs
git commit -m "mailcore: refresh_local_contacts miner over synced mail"
```

---

### Task 3: Graph `/me/people` client call

**Files:**
- Modify: `mailcore/src/graph/client.rs` (add `Person` + `GraphClient::people`; add tests)

**Interfaces:**
- Produces:
  - `pub struct Person { pub name: String, pub address: String, pub rank: i64 }`
  - `pub fn people(&self) -> Result<Vec<Person>, GraphError>` — fetches up to 200 relevance-ranked people; `rank` is the 0-based position in the relevance order. Errors (403, offline, parse) propagate — the caller (Task 4) decides how to degrade.

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/graph/client.rs` tests (mirror the module's existing `FakeServer`/`Route` test style, e.g. `create_draft_posts_body_and_parses_returned_draft`):

```rust
    #[test]
    fn people_parses_ranked_directory_results() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_contains: "/me/people".into(),
            status: 200,
            body: r#"{"value":[
                {"displayName":"Ann Lee","scoredEmailAddresses":[{"address":"ann@x.com","relevanceScore":9.0}]},
                {"displayName":"No Email Person","scoredEmailAddresses":[]},
                {"displayName":"Bob Jones","scoredEmailAddresses":[{"address":"bob@x.com","relevanceScore":8.0}]}
            ]}"#.into(),
        }]);
        let client = GraphClient::new(srv.base_url(), "TOKEN".into()); // adapt to the real constructor used by neighbouring tests
        let people = client.people().unwrap();
        // The address-less entry is skipped; rank is the position in the response.
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].address, "ann@x.com");
        assert_eq!(people[0].rank, 0);
        assert_eq!(people[1].address, "bob@x.com");
        assert_eq!(people[1].rank, 2); // original index preserved (the skipped entry was #1)
    }

    #[test]
    fn people_propagates_a_403_as_an_error() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(), path_contains: "/me/people".into(),
            status: 403, body: r#"{"error":{"code":"Authorization_RequestDenied"}}"#.into(),
        }]);
        let client = GraphClient::new(srv.base_url(), "T".into());
        assert!(client.people().is_err()); // caller (engine) degrades; the client just surfaces the error
    }
```

Note: match the exact `GraphClient` constructor, `FakeServer`, and `Route` field names/spelling the surrounding tests already use — adapt the two `GraphClient::new(...)`/`Route{...}` calls above to them; keep the assertions.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore people_parses people_propagates`
Expected: FAIL — `no method named people`.

- [ ] **Step 3: Implement**

Add the struct near the other model structs in `client.rs` (or import from `graph::model` if that's where `Message`/`Recipient` live — place it wherever `Recipient` is defined and re-export as needed; keep it `pub`):

```rust
/// One entry from Graph `/me/people`: a display name, the person's primary
/// email address, and their 0-based rank in the relevance-ordered response
/// (0 = most relevant).
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub name: String,
    pub address: String,
    pub rank: i64,
}
```

Add the method to `impl GraphClient`:

```rust
    /// GET `/me/people` (top 200, relevance-ordered). Each returned person's
    /// primary address is its first `scoredEmailAddresses` entry; people with
    /// no email address are skipped. `rank` is the entry's position in the
    /// original response order, preserved across the skips. Requires the
    /// `People.Read` scope — a token without it yields a 403, which surfaces
    /// as an `Err` for the caller to degrade on.
    pub fn people(&self) -> Result<Vec<Person>, GraphError> {
        let resp = self.send(
            Method::Get,
            "/me/people?$top=200&$select=displayName,scoredEmailAddresses",
            None,
            &[],
        )?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        let people = items
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let addr = p
                    .get("scoredEmailAddresses")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|e| e.get("address"))
                    .and_then(Value::as_str)?;
                let name = p.get("displayName").and_then(Value::as_str).unwrap_or("");
                Some(Person { name: name.to_string(), address: addr.to_string(), rank: i as i64 })
            })
            .collect();
        Ok(people)
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore people_parses people_propagates`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs
git commit -m "mailcore: GraphClient::people (/me/people, ranked)"
```

---

### Task 4: Auth scope bump + sync-engine wiring

**Files:**
- Modify: `mailcore/src/auth.rs` (scope constant + the test asserting it)
- Modify: `mailcore/src/sync/engine.rs` (call `refresh_local_contacts` each pass; `people()` on the full pass, best-effort; add a test)

**Interfaces:**
- Consumes: `Store::refresh_local_contacts` (Task 2), `Store::upsert_contact` (Task 1), `GraphClient::people` (Task 3).

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/sync/engine.rs` tests (mirror the module's existing `FakeServer`-driven engine tests, e.g. the delta-sync tests around `send_draft_of_a_local_draft…`). Seed a folder + a message, serve `/me/people`, run one full sync pass, and assert contacts got populated from BOTH sources:

```rust
    #[test]
    fn sync_pass_populates_contacts_from_local_mail_and_people() {
        // ... build the engine + FakeServer the same way the other engine tests do,
        // serving: list folders, one folder delta with a message from alice@x /
        // to bob@x, and GET /me/people returning carol@x. Run one full pass. ...
        // Local mining captured the correspondents:
        assert!(store.search_contacts("alice", 1).unwrap().first().is_some());
        assert!(store.search_contacts("bob", 1).unwrap().first().is_some());
        // /me/people augmented with an org person the user never emailed:
        let carol = store.search_contacts("carol", 1).unwrap();
        assert_eq!(carol.first().map(|c| c.address.as_str()), Some("carol@x.com"));
        assert_eq!(carol[0].source, "graph");
    }

    #[test]
    fn people_403_does_not_fail_the_sync_pass() {
        // ... same setup but /me/people returns 403. Run a full pass. ...
        // The pass still completes and local contacts are present; no panic, no error surfaced.
        assert!(store.search_contacts("alice", 1).unwrap().first().is_some());
    }
```

Note: build the engine/`FakeServer`/routes exactly as the neighbouring engine tests do (routes for `list_folders`, the folder `delta`, and now `/me/people`); keep the contact assertions. If exercising a full engine pass is heavy, an acceptable alternative is to test the two new engine helpers (`refresh_contacts`/`sync_people` below) directly against a seeded store + `FakeServer`.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore sync_pass_populates_contacts people_403_does_not_fail`
Expected: FAIL (contacts empty — no wiring yet).

- [ ] **Step 3: Implement**

In `mailcore/src/auth.rs`, change the scope string in **both** places it appears (the `Default` impl ~line 31 and the test ~line 266) from
`"Mail.ReadWrite offline_access"` to `"Mail.ReadWrite People.Read offline_access"`.

In `mailcore/src/sync/engine.rs`, at the end of a successful `sync_pass` (after the per-folder deltas complete — locate the point where `sync_pass` has finished the folder loop and is about to return success), add:

```rust
        // Refresh the local contact index from whatever mail is now stored
        // (cheap, idempotent). Best-effort: a failure here must not fail the pass.
        let _ = self.store.refresh_local_contacts();

        // On a full pass (folders were re-enumerated, e.g. after sign-in),
        // augment contacts from the corporate directory. Best-effort and
        // gracefully degrading: People.Read may be denied (403), blocked by
        // Conditional Access, or offline — in every case we skip it silently
        // and the locally-mined contacts still power autocomplete.
        if include_folders {
            self.sync_people();
        }
```

Add the helper to the engine's `impl` (near `sync_pass`):

```rust
    /// Best-effort `/me/people` fetch → contact upserts. Any error (insufficient
    /// scope, Conditional Access, offline, parse) is swallowed: directory
    /// suggestions are a bonus on top of local contacts, never a hard
    /// dependency, so this never fails a sync pass or emits an error event.
    fn sync_people(&mut self) {
        let people = match self.with_auth(|c| c.people()) {
            Ok(p) => p,
            Err(_) => return, // graceful degradation — local contacts carry on
        };
        for p in people {
            let _ = self.store.upsert_contact(&crate::store::Contact {
                name: p.name,
                address: p.address,
                source: "graph".to_string(),
                last_seen: String::new(),
                frequency: 0,
                relevance: Some(p.rank),
            });
        }
    }
```

Notes for the implementer:
- `with_auth(|c| ...)` is the existing engine wrapper that supplies an authed `GraphClient` (used by `list_folders`/`delta`/etc.) — use whatever that method is actually named in this file; match the existing call sites.
- `include_folders` is the existing `sync_pass` parameter that is `true` on a full pass. If `sync_pass`'s success tail isn't a single obvious spot, add the two blocks right before it returns `true`/success.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore sync_pass_populates_contacts people_403_does_not_fail`
Expected: PASS. Then `bash "$LCARGO" test -p mailcore` — all green (existing engine/auth tests unaffected; the scope test now asserts the new string).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/auth.rs mailcore/src/sync/engine.rs
git commit -m "mailcore: sync contacts each pass (local miner + best-effort /me/people); scope += People.Read"
```

---

### Task 5: Bcc — store column

**Files:**
- Modify: `mailcore/src/store/schema.rs` (add `bcc_recipients` to the `messages` CREATE TABLE)
- Modify: `mailcore/src/store/mod.rs` (ALTER-TABLE migration; `MessageRow.bcc_recipients`; append the column to every `messages` SELECT that feeds `map_message_row`; `map_message_row`; `update_draft_fields` gains a `bcc` param; add a test)

**Interfaces:**
- Produces: `MessageRow` gains `pub bcc_recipients: String` (last field); `update_draft_fields(&self, id, subject, to, cc, bcc, body_html)` (bcc inserted before `body_html`).

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/store/mod.rs` tests (extend the style of `update_draft_fields_changes_subject_and_body`):

```rust
    #[test]
    fn update_draft_fields_persists_bcc() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("", "", "", "").unwrap(); // matches the existing local-draft test setup
        s.update_draft_fields(&id, "Sub", "to@x", "cc@x", "bcc@x", "body").unwrap();
        let row = s.draft(&id).unwrap();
        assert_eq!(row.to_recipients, "to@x");
        assert_eq!(row.cc_recipients, "cc@x");
        assert_eq!(row.bcc_recipients, "bcc@x");
    }
```

(Adapt `create_local_draft`/`draft` calls to the exact existing signatures used by `update_draft_fields_changes_subject_and_body`.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore update_draft_fields_persists_bcc`
Expected: FAIL — `update_draft_fields` takes 5 args / `no field bcc_recipients`.

- [ ] **Step 3: Implement**

In `schema.rs`, add to the `messages` CREATE TABLE (next to `to_recipients`/`cc_recipients`):
```sql
    bcc_recipients  TEXT NOT NULL DEFAULT '',
```

In `mod.rs`, add an ALTER-TABLE migration next to the existing ones (the `is_draft`/`body_html` `ALTER TABLE ... ADD COLUMN` block, ~lines 371-379). Follow that exact pattern (it ignores the "duplicate column" error for DBs already having it):
```rust
        let _ = self.conn.execute(
            "ALTER TABLE messages ADD COLUMN bcc_recipients TEXT NOT NULL DEFAULT ''",
            [],
        );
```

Add the field to `MessageRow`, as the **last** field:
```rust
    pub is_draft: bool,
    pub bcc_recipients: String,
```

In `map_message_row`, add (as the last mapping, index 16):
```rust
        bcc_recipients: row.get(16)?,
```

Append `, bcc_recipients` to the **column list of every SELECT that maps through `map_message_row`**. Grep for the string `is_draft` inside `SELECT` statements in `mod.rs` — each such SELECT currently ends its column list with `... preview, is_draft`; change each to `... preview, is_draft, bcc_recipients`. These are: `messages_in_folder`, `conversations_in_folder` (both the CTE's inner column list *and* the outer `SELECT k....` list — append `k.bcc_recipients` there), `draft`, and `search`. (Do NOT touch `upsert_message`'s INSERT — synced messages have no bcc; the column defaults to `''`.)

Change `update_draft_fields` to take `bcc` and write it:
```rust
    pub fn update_draft_fields(
        &self,
        id: &str,
        subject: &str,
        to: &str,
        cc: &str,
        bcc: &str,
        body_html: &str,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE messages SET subject = ?2, to_recipients = ?3, cc_recipients = ?4, bcc_recipients = ?5
             WHERE id = ?1",
            params![id, subject, to, cc, bcc],
        )?;
        tx.execute(
            "INSERT INTO bodies (message_id, content_type, content) VALUES (?1, 'html', ?2)
             ON CONFLICT(message_id) DO UPDATE SET content = excluded.content",
            params![id, body_html],
        )?;
        tx.commit()?;
        Ok(())
    }
```

This changes `update_draft_fields`'s signature — its existing callers (in `lookxy` app compose save, and the existing store test `update_draft_fields_changes_subject_and_body`) must pass a `bcc` argument. Update the existing store test to pass `""` for bcc. The lookxy caller is updated in Task 7; to keep the workspace compiling after THIS task, also update the lookxy call site minimally to pass `""` for now (Task 7 replaces it with the real `compose.bcc`). Grep `update_draft_fields(` across the repo and fix every call.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore update_draft_fields_persists_bcc`
Expected: PASS. Then `bash "$LCARGO" test -p mailcore` and `bash "$LCARGO" build -p lookxy` — both green (all SELECTs still map correctly; the lookxy call site compiles).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore -p lookxy --all-targets -- -D warnings
git add mailcore/src/store/schema.rs mailcore/src/store/mod.rs lookxy/src
git commit -m "mailcore: bcc_recipients column + MessageRow + update_draft_fields(bcc)"
```

---

### Task 6: Bcc — Graph draft/send

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`draft_body_json`, `create_draft`, `update_draft` gain `bcc`; add/extend a test)
- Modify: `mailcore/src/sync/outbox.rs` (`ensure_draft_on_graph` reads `bcc_recipients`)

**Interfaces:**
- Consumes: `MessageRow.bcc_recipients` (Task 5), `parse_recipients` (pub(crate), Task 2).
- Produces: `create_draft(body_html, subject, to, cc, bcc)`, `update_draft(id, body_html, subject, to, cc, bcc)`, `draft_body_json(body_html, subject, to, cc, bcc)`.

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/graph/client.rs` tests (or extend `create_draft_posts_body_and_parses_returned_draft`) — assert the POSTed body includes `bccRecipients` with the bcc address:

```rust
    #[test]
    fn create_draft_includes_bcc_recipients() {
        // ... FakeServer capturing the POST /me/messages body, as the existing
        // create_draft test does ...
        let to = [Recipient { name: "B".into(), address: "b@x".into() }];
        let bcc = [Recipient { name: "S".into(), address: "secret@x".into() }];
        let _ = client.create_draft("<p>hi</p>", "Sub", &to, &[], &bcc);
        let body = srv.last_request_body(); // adapt to how the existing test reads the captured body
        let sent = mailcore::json::parse(&body).unwrap();
        let bccs = sent.get("bccRecipients").and_then(mailcore::json::Value::as_array).unwrap();
        assert_eq!(bccs.len(), 1);
        // the nested emailAddress.address is "secret@x"
        assert_eq!(
            bccs[0].get("emailAddress").and_then(|e| e.get("address")).and_then(mailcore::json::Value::as_str),
            Some("secret@x")
        );
    }
```

(Adapt the FakeServer/captured-body access to the existing `create_draft` test's mechanism.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore create_draft_includes_bcc`
Expected: FAIL — `create_draft` takes 4 recipient-less args / no `bccRecipients` in body.

- [ ] **Step 3: Implement**

In `client.rs`, add a `bcc` parameter to `draft_body_json`, `create_draft`, and `update_draft`, and emit `bccRecipients`:

```rust
fn draft_body_json(body_html: &str, subject: &str, to: &[Recipient], cc: &[Recipient], bcc: &[Recipient]) -> String {
    Value::Object(vec![
        ("subject".to_string(), Value::Str(subject.to_string())),
        (
            "body".to_string(),
            Value::Object(vec![
                ("contentType".to_string(), Value::Str("HTML".to_string())),
                ("content".to_string(), Value::Str(body_html.to_string())),
            ]),
        ),
        ("toRecipients".to_string(), recipients_json(to)),
        ("ccRecipients".to_string(), recipients_json(cc)),
        ("bccRecipients".to_string(), recipients_json(bcc)),
    ])
    .to_string()
}
```

`create_draft(&self, body_html, subject, to, cc, bcc: &[Recipient])` and `update_draft(&self, id, body_html, subject, to, cc, bcc: &[Recipient])` — thread `bcc` into the `draft_body_json(...)` call in each.

In `outbox.rs` `ensure_draft_on_graph`, after the existing `to`/`cc` parse lines, add bcc and pass it:
```rust
    let to = parse_recipients(&row.to_recipients);
    let cc = parse_recipients(&row.cc_recipients);
    let bcc = parse_recipients(&row.bcc_recipients);
```
and update both call sites in that function: `create_draft(&body.content, &row.subject, &to, &cc, &bcc)` and `update_draft(id, &body.content, &row.subject, &to, &cc, &bcc)`.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore create_draft_includes_bcc`
Expected: PASS. Then `bash "$LCARGO" test -p mailcore` — green (existing draft/outbox tests: update their `create_draft`/`update_draft` calls to pass `&[]` for bcc where needed).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs mailcore/src/sync/outbox.rs
git commit -m "mailcore: bccRecipients in draft create/update + outbox send"
```

---

### Task 7: Bcc — compose field

**Files:**
- Modify: `lookxy/src/ui/compose.rs` (`Compose.bcc`, `ComposeField::Bcc`, `new`, `cycle_focus`, `draw`, `handle_key`)
- Modify: `lookxy/src/app.rs` (compose load/save wiring reads/writes bcc)

**Interfaces:**
- Consumes: `update_draft_fields(id, subject, to, cc, bcc, body)` (Task 5), `MessageRow.bcc_recipients` (Task 5).

- [ ] **Step 1: Write the failing test**

Add to `lookxy/src/ui/compose.rs` tests (mirror existing compose tests):

```rust
    #[test]
    fn focus_cycle_includes_bcc_between_cc_and_subject() {
        let mut c = Compose::new("d".into());
        assert_eq!(c.focus, ComposeField::To);
        cycle_focus(&mut c); assert_eq!(c.focus, ComposeField::Cc);
        cycle_focus(&mut c); assert_eq!(c.focus, ComposeField::Bcc);
        cycle_focus(&mut c); assert_eq!(c.focus, ComposeField::Subject);
        cycle_focus(&mut c); assert_eq!(c.focus, ComposeField::Body);
        cycle_focus(&mut c); assert_eq!(c.focus, ComposeField::To);
    }
```

Also add an app-level round-trip test (in `app.rs` tests) that a saved compose's bcc reaches the store — model it on the existing compose save test if one exists; otherwise assert `store.draft(id).bcc_recipients` after driving a save with a bcc value. (If the existing save path is only reachable through `apply_compose_action`, set `app.compose = Some(Compose { bcc: "secret@x".into(), .. })` and a `ComposeAction::Save`, call `app.apply_compose_action()`, then read the draft.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy focus_cycle_includes_bcc`
Expected: FAIL — `no variant Bcc` / `no field bcc`.

- [ ] **Step 3: Implement**

In `compose.rs`:
- `ComposeField` enum: add `Bcc` (between `Cc` and `Subject`).
- `Compose` struct: add `pub bcc: String`.
- `Compose::new`: add `bcc: String::new()`.
- `cycle_focus`: To→Cc→**Bcc**→Subject→Body→To.
- `draw`: add a Bcc field row between Cc and Subject (copy the `draw_field(... "Cc" ... compose.cc ...)` call, add one for `"Bcc"`/`compose.bcc`/`focus == Bcc`; adjust the vertical layout to include the extra row).
- `handle_key`: in the `KeyCode::Char(c)` and `KeyCode::Backspace` matches, add a `ComposeField::Bcc => compose.bcc.push(c)` / `compose.bcc.pop()` arm alongside `To`/`Cc`.

In `app.rs`:
- Where a draft is loaded into a `Compose` (the reply/forward/resume path that reads `store.draft(id)` and fills `compose.to`/`compose.cc`): also set `compose.bcc` from the draft row's `bcc_recipients`.
- Where compose is saved (`apply_compose_action` calling `update_draft_fields`): pass `compose.bcc` in the new `bcc` position (replacing the `""` placeholder Task 5 added).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy focus_cycle_includes_bcc` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/compose.rs lookxy/src/app.rs
git commit -m "lookxy: Bcc compose field (focus, draw, edit, load/save)"
```

---

### Task 8: Recipient autocomplete dropdown

**Files:**
- Modify: `lookxy/src/ui/compose.rs` (`Autocomplete` state, token extraction, dropdown keys + draw, `search_contacts` wiring)

**Interfaces:**
- Consumes: `Store::search_contacts(query, limit)` (Task 1); `Compose` To/Cc/Bcc fields (Task 7); `mailcore::store::Contact`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/ui/compose.rs` tests:

```rust
    #[test]
    fn current_token_is_text_after_the_last_separator() {
        assert_eq!(current_token("a@x; bo"), "bo");
        assert_eq!(current_token("a@x;"), "");
        assert_eq!(current_token("  al"), "al");
        assert_eq!(current_token("a@x, c"), "c"); // comma is also a separator
        assert_eq!(current_token(""), "");
    }

    #[test]
    fn accepting_a_match_replaces_the_current_token() {
        // field holds one finished recipient plus a partial token being typed
        let field = "bob@x; al".to_string();
        let contact = mailcore::store::Contact {
            name: "Alice".into(), address: "alice@x.com".into(), source: "local".into(),
            last_seen: "".into(), frequency: 1, relevance: None,
        };
        let result = apply_completion(&field, &contact);
        // the partial "al" is replaced with the structured recipient; earlier ones untouched
        assert_eq!(result, "bob@x; Alice <alice@x.com>; ");
    }

    #[test]
    fn dropdown_navigation_is_clamped() {
        let mut ac = Autocomplete {
            field: ComposeField::To, query: "a".into(),
            matches: vec![sample_contact("a@x"), sample_contact("b@x")], index: 0,
        };
        ac.move_selection(-1); assert_eq!(ac.index, 0); // clamped at top
        ac.move_selection(1);  assert_eq!(ac.index, 1);
        ac.move_selection(1);  assert_eq!(ac.index, 1); // clamped at bottom
    }
```

Add a small `fn sample_contact(addr: &str) -> mailcore::store::Contact` test helper.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy current_token accepting_a_match dropdown_navigation`
Expected: FAIL — `cannot find function current_token`/`apply_completion`/type `Autocomplete`.

- [ ] **Step 3: Implement**

In `compose.rs`, add the state + pure helpers:

```rust
use mailcore::store::Contact;

/// The open autocomplete dropdown over a recipient field.
pub struct Autocomplete {
    pub field: ComposeField,
    pub query: String,
    pub matches: Vec<Contact>,
    pub index: usize,
}

impl Autocomplete {
    /// Move the highlighted suggestion, clamped (no wrap).
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let max = (self.matches.len() - 1) as isize;
        self.index = (self.index as isize + delta).clamp(0, max) as usize;
    }
}

/// The recipient token currently being typed: the text after the last `;` or
/// `,` in the field, trimmed. Empty when the field ends in a separator (or is
/// empty) — meaning nothing to complete.
pub(crate) fn current_token(field: &str) -> String {
    field
        .rsplit(|c| c == ';' || c == ',')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Replace the current (last) token in `field` with the chosen contact as a
/// structured `Name <addr>; ` (ready for the next recipient), leaving any
/// earlier finished recipients intact.
pub(crate) fn apply_completion(field: &str, c: &Contact) -> String {
    let cut = field.rfind(|ch| ch == ';' || ch == ',').map(|i| i + 1).unwrap_or(0);
    let prefix = &field[..cut];
    let sep = if prefix.is_empty() { "" } else { " " };
    let name = c.name.trim();
    let rendered = if name.is_empty() {
        c.address.clone()
    } else {
        format!("{} <{}>", name, c.address)
    };
    format!("{prefix}{sep}{rendered}; ")
}
```

Add `pub autocomplete: Option<Autocomplete>` to `Compose` (and `autocomplete: None` in `Compose::new`).

Wire it into key handling. Refactor `handle_key(app, key)` so the compose-only mutation happens against `app.compose`, then a store-backed refresh runs with a fresh borrow:

- At the point a recipient field (`To`/`Cc`/`Bcc`) receives a `Char`/`Backspace`, after mutating the field, record that a refresh is needed for that field.
- When `app.compose.as_ref()`'s `autocomplete.is_some()`, handle these FIRST (before the plain field edits): `Down`→`move_selection(1)`, `Up`→`move_selection(-1)`, `Enter` or `Tab`→accept (`let c = matches[index].clone(); field := apply_completion(field, &c); autocomplete = None`). **`Esc` is intercepted at the top of `handle_key`** (today it unconditionally sets `ComposeAction::Save`) — change that interceptor to first close an open dropdown and return, and only fall through to Save when no dropdown is open. Do NOT put an `Esc` arm inside `handle_compose_key`; it would be unreachable.
- When `autocomplete` is `None`, `Tab` keeps cycling focus (today's behavior).
- After the compose borrow ends, if a refresh was requested for field F: read the field text, compute `current_token`; if non-empty, `let matches = app.store.search_contacts(&token, 8).unwrap_or_default();` and set `app.compose`'s `autocomplete = Some(Autocomplete { field: F, query: token, matches, index: 0 })` (or `None` if `matches` is empty); if the token is empty, set `autocomplete = None`.

Concretely, structure it as:

```rust
pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Esc: close an open autocomplete dropdown if there is one; otherwise the
    // existing "save the draft" behavior. (This REPLACES today's unconditional
    // `Esc => ComposeAction::Save` block at the top of handle_key.)
    if key.code == KeyCode::Esc {
        if let Some(compose) = app.compose.as_mut() {
            if compose.autocomplete.is_some() {
                compose.autocomplete = None;
                return;
            }
        }
        app.compose_action = Some(ComposeAction::Save);
        return;
    }
    // ... existing Ctrl handling unchanged ...
    let refresh = {
        let Some(compose) = app.compose.as_mut() else { return; };
        handle_compose_key(compose, key) // returns Option<ComposeField> when a recipient field's text changed
    };
    if let Some(field) = refresh {
        refresh_autocomplete(app, field);
    }
}

/// Returns Some(field) when `field`'s recipient text changed and the dropdown
/// should be refreshed by the caller (which has the store). All autocomplete
/// navigation/accept/dismiss that needs no store is handled here directly.
fn handle_compose_key(compose: &mut Compose, key: KeyEvent) -> Option<ComposeField> {
    // If a dropdown is open, its keys take precedence:
    if compose.autocomplete.is_some() {
        match key.code {
            KeyCode::Down => { compose.autocomplete.as_mut().unwrap().move_selection(1); return None; }
            KeyCode::Up   => { compose.autocomplete.as_mut().unwrap().move_selection(-1); return None; }
            // Esc is handled by handle_key's top-level interceptor, not here.
            KeyCode::Enter | KeyCode::Tab => {
                let ac = compose.autocomplete.take().unwrap();
                if let Some(c) = ac.matches.get(ac.index).cloned() {
                    let field = recipient_field_mut(compose, ac.field);
                    *field = apply_completion(field, &c);
                }
                return None;
            }
            _ => {}
        }
    }
    // ... the existing Tab(cycle)/Char/Backspace/Body-editing match ...
    // In the To/Cc/Bcc Char and Backspace arms, after mutating the field,
    // `return Some(compose.focus);` so the caller refreshes suggestions.
    // Subject/Body arms and everything else `return None;`.
}

fn recipient_field_mut(compose: &mut Compose, field: ComposeField) -> &mut String {
    match field {
        ComposeField::To => &mut compose.to,
        ComposeField::Cc => &mut compose.cc,
        ComposeField::Bcc => &mut compose.bcc,
        _ => &mut compose.to, // unreachable: refresh only fires for recipient fields
    }
}

fn refresh_autocomplete(app: &mut App, field: ComposeField) {
    let token = {
        let Some(compose) = app.compose.as_ref() else { return; };
        current_token(match field {
            ComposeField::To => &compose.to,
            ComposeField::Cc => &compose.cc,
            ComposeField::Bcc => &compose.bcc,
            _ => return,
        })
    };
    let matches = if token.is_empty() { Vec::new() } else { app.store.search_contacts(&token, 8).unwrap_or_default() };
    if let Some(compose) = app.compose.as_mut() {
        compose.autocomplete = if matches.is_empty() {
            None
        } else {
            Some(Autocomplete { field, query: token, matches, index: 0 })
        };
    }
}
```

Draw the dropdown: in compose `draw`, after drawing the focused recipient field, if `compose.autocomplete` is `Some` and its `field == compose.focus`, render a small bordered overlay directly below that field listing the matches (`Name <addr>`), highlighting `index`. Bound its height to the match count. (Reuse `Clear` + a `List` with a highlight style, as `message_list::draw_move_picker` does.)

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy current_token accepting_a_match dropdown_navigation` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green (existing compose Tab-cycles-focus / typing tests still pass — Tab only accepts when a dropdown is open).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/compose.rs
git commit -m "lookxy: recipient autocomplete dropdown over To/Cc/Bcc"
```

---

## Notes for the implementer

- **Match existing signatures/helpers.** Several tasks say "adapt to the existing X" (FakeServer/Route field names, `GraphClient` constructor, `create_local_draft`/`draft` signatures, the engine's `with_auth`/`sync_pass` shape, the compose draft-load site). Read the neighbouring code and match it exactly; keep the assertions as written.
- **Flat/threaded mail and existing compose must stay green.** Autocomplete only acts while a dropdown is open (Tab still cycles focus otherwise); Bcc is additive. If an existing test breaks, the change leaked — fix the change, not the test.
- **Graceful degradation is load-bearing (Task 4).** `sync_people` swallowing all errors is intentional and required — do not turn a `/me/people` failure into a surfaced error.
- **Deferred (out of scope, noted for the final review):** the spec's user-facing "directory unavailable — re-sign-in" notice is trimmed from this plan — the feature degrades silently to local contacts (which is the load-bearing behavior). Add the notice later if desired.
