# lookxy categories (color labels) — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-20.
**Builds on:** the message model/store (`Message`/`MessageRow`, `MESSAGE_SELECT`,
the idempotent-migration + flat-list-encoding patterns), the outbox mutation
path (`SyncCommand::MarkRead`/`SetFlag` → `enqueue_and_drain(OutboxOp::…)` →
`sync::outbox::apply_op`), the best-effort full-pass augmentation
(`sync_people`), the popup pattern (`ui/attachments.rs` +
`App::open_attachments_popup`/key routing), and the search-style filtered view
(`App::search`/`SearchState` + `reload_messages`).

## Goal

Let the user assign/clear Outlook **categories** (named color labels) on a
message, see them as colored dots in the message list and colored chips in the
reader, and filter a folder to one category. Backed by Graph
`message.categories` (a `Vec<String>` of category names) and the mailbox's
master category list (`/me/outlook/masterCategories`, each an `outlookCategory`
with a `displayName` and a `color`).

## Background

A message carries a plain list of category **names** (`categories: ["Work",
"Urgent"]`). The names' colors live separately in the master category list —
each `outlookCategory` has a `displayName` and a `color` that is one of
`"preset0"`…`"preset24"` or `"none"`. Categories are defined in Outlook; lookxy
consumes the master list (for the picker's choices and the display colors) and
sets the per-message name list, but does not create/rename/delete master
categories (see YAGNI).

## Product decisions (locked)

- **List display:** one colored `●` per category, before the subject, each in
  its category's color (best-effort terminal approximation; unknown → gray).
- **Reader display:** a `Categories: [Work] [Urgent]` header line (each name in
  its color) when the message has any.
- **Keys:** **`l`** opens the picker in Assign mode; **`L`** opens it in Filter
  mode. (Both free in `on_key_char`; categories are a Mail-mode feature.)
- **One popup, two modes** — Assign toggles categories on the selected message;
  Filter picks one category to filter the folder view by.
- **Outbox-backed writes** — setting categories is an optimistic-local +
  queued-Graph-op mutation, same resilience as mark-read/flag.
- **No master-list editing**, no bulk multi-message assign, no per-category
  counts.

## Architecture

### 1. Model + store (`mailcore`)

- `Message.categories: Vec<String>`, parsed in `from_json` from the Graph
  `categories` string array (absent → empty). Add `categories` to
  `MESSAGE_SELECT` so delta pages carry it.
- `MessageRow.categories: Vec<String>` (persisted/read).
- New `MasterCategory { display_name: String, color: String }` with
  `from_json` reading `displayName`/`color` from an `outlookCategory`.
- Store:
  - `messages.categories TEXT` column (idempotent `ALTER TABLE … ADD COLUMN`
    migration, same pattern as `is_meeting_request`). Encoded as a flat string
    via the existing recipients-style delimiter encoding
    (`encode_categories`/`decode_categories`, mirroring
    `encode_recipients`/`decode_recipients` so a name containing the delimiter
    can't corrupt the list). `upsert_message` writes it; every `map_message_row`
    SELECT reads it; `map_message_row` decodes it.
  - `Store::set_categories(id, &[String])` — the optimistic-local write (updates
    just the `categories` column), mirroring `set_read`/`set_flag`.
  - `master_categories(display_name TEXT PRIMARY KEY, color TEXT)` table;
    `Store::replace_master_categories(&[MasterCategory])` (replace-all in one
    transaction) and `Store::master_categories() -> Vec<MasterCategory>`.

### 2. Graph client (`mailcore/src/graph/client.rs`)

- `get_master_categories(&self) -> Result<Vec<MasterCategory>, GraphError>` —
  GET `/me/outlook/masterCategories`, parse the `value` array.
- `set_message_categories(&self, id, categories: &[String]) -> Result<(), GraphError>`
  — PATCH `/me/messages/{id}` with `{"categories": [ …names… ]}`.

### 3. Sync (`mailcore/src/sync/engine.rs` + `sync/outbox.rs`)

- `SyncCommand::SetCategories { id: String, categories: Vec<String> }`:
  optimistic `store.set_categories(&id, &categories)` +
  `enqueue_and_drain(OutboxOp::SetCategories { id, categories })`. `apply_op`
  handles `SetCategories` via `set_message_categories`. On quarantine (repeated
  4xx), the existing mail reconverge (clear delta links + full re-upsert) pulls
  the true categories back — no extra handling needed, since categories live on
  the message row that reconverge re-fetches.
- Master list: `SyncCommand::RefreshCategories` (on-demand, e.g. when opening
  the picker) and a best-effort fetch folded into the full sync pass (like
  `sync_people`): `get_master_categories` → `store.replace_master_categories` →
  `SyncEvent::CategoriesUpdated`. A failure degrades silently (dots fall back to
  gray; the picker shows the cached list).
- `SyncEvent::CategoriesUpdated` (master list changed; the UI re-reads it).
  Per-message category changes ride the existing `MessagesUpdated` re-read.

### 4. Display (`lookxy`)

- `App` holds a `master_categories: Vec<MasterCategory>` (loaded from the store
  on `CategoriesUpdated` and at startup) and derives a `name → Color` lookup.
- **List** (`ui/message_list.rs`): `child_line`/`line` prepend one `●` span per
  category (in `Categories`' declared order) before the subject, colored via the
  lookup (`preset→Color`; unknown/`none` → `Color::Gray`). Rendered as separate
  colored `Span`s (the row becomes a multi-span `Line`).
- **Reader** (`ui/reading.rs`): `header_lines` appends a `Categories:` line with
  one colored `[name]` chip per category when non-empty.
- **Color mapping** (`ui/categorypicker.rs` or a small `ui` helper):
  `preset_color(&str) -> Color` maps `preset0..preset24` to a fixed palette of
  ratatui named colors (best-effort 16-color approximations), `"none"`/unknown →
  `Color::Gray`.

### 5. Assign + filter popup (`lookxy/src/ui/categorypicker.rs` + `app.rs`)

- `CategoryPicker { mode: PickerMode, items: Vec<CategoryItem>, index: usize }`
  where `PickerMode { Assign, Filter }` and `CategoryItem { name, color,
  selected }` (`selected` = "on this message" in Assign mode; unused in Filter).
- `App::category_picker: Option<CategoryPicker>`; `App::category_filter:
  Option<String>`.
- **`l` → `open_category_picker(Assign)`**: builds items from
  `master_categories`, each `selected` iff the highlighted message's
  `categories` contains it; also fires `RefreshCategories` so the list is fresh.
  `j`/`k` move, `Space` toggles the highlighted item's `selected`, `Enter`
  applies (send `SetCategories { id, categories: <selected names> }`, close),
  `Esc` cancels.
- **`L` → `open_category_picker(Filter)`**: `Enter` sets `category_filter =
  Some(name)` and `reload_messages`; `Esc` cancels; a status-bar hint shows the
  active filter, and pressing `L`→Esc or a dedicated clear (e.g. `Esc` in the
  folder view when a filter is active) clears it.
- `reload_messages`: when `category_filter` is `Some(name)`, filter the
  folder's messages in memory (retain those whose `categories` contains `name`)
  after the existing `messages_in_folder` query — no new store method. An active
  filter forces the flat list (threaded interaction with the filter is out for
  v1): `reload_messages` takes the flat path whenever `category_filter.is_some()`.
- `is_capturing_text` is unaffected (the picker is list-navigation, not text
  entry).
- Key routing: the picker gets keys ahead of the panes when open (same
  precedence `attachments` gets).

## Data flow

```
assign:  l → open_category_picker(Assign) + RefreshCategories
             → Space toggles items → Enter
             → SetCategories{id, names}  (optimistic store.set_categories + outbox)
             → MessagesUpdated re-read → dots/chips reflect it
filter:  L → open_category_picker(Filter) → Enter(name)
             → category_filter=Some(name) → reload_messages (only matching) 
master:  full sync pass / RefreshCategories
             → get_master_categories → replace_master_categories → CategoriesUpdated
             → app reloads master list + color map
```

## Error handling & edge cases

- **Set fails** → outbox retry/quarantine; a quarantine triggers the existing
  mail reconverge (categories live on the re-fetched message row).
- **Master-list fetch fails** → silent degradation: dots/chips use `Color::Gray`,
  the picker shows the cached master list (possibly empty).
- **Empty master list** → the Assign picker shows "(no categories — define them
  in Outlook)"; Filter picker likewise has nothing to pick.
- **Category on a message but not in the master list** (defined then deleted, or
  a shared-mailbox category) → still displayed (gray dot / gray chip) and still
  toggleable off in Assign mode (it appears as a selected item synthesized from
  the message when absent from the master list).
- **Filter active, then folder changes** → `category_filter` persists across
  folder switches until cleared (a deliberate "show me Work everywhere" behavior);
  cleared by `Esc` in the folder view when set, or re-picking.

## Testing

**mailcore (unit):**
- `Message::from_json` parses `categories` (present array; absent → empty).
- `MasterCategory::from_json` reads `displayName`/`color`.
- Store round-trips `messages.categories` (incl. a name containing the
  delimiter); the migration is idempotent; `set_categories` updates only that
  column; `replace_master_categories`/`master_categories` round-trip and replace.
- Client `get_master_categories` (FakeServer GET) parses names+colors; client
  `set_message_categories` (FakeServer PATCH) sends `{"categories":[…]}`.
- Engine `SetCategories`: optimistic store write + a PATCH on drain; master-list
  fetch on the full pass stores the list + emits `CategoriesUpdated`.

**lookxy (unit):**
- `open_category_picker(Assign)` seeds items from the highlighted message
  (selected iff present) and fires `RefreshCategories`; `Space`+`Enter` sends
  `SetCategories` with exactly the toggled set.
- `open_category_picker(Filter)` + `Enter` sets `category_filter` and
  `reload_messages` yields only matching messages; `Esc`/clear removes it.
- List renders one `●` per category; reader renders `Categories:` chips; picker
  renders items with `[x]`/`[ ]` (Assign) — all via `TestBackend`.
- `preset_color` maps a few presets to distinct colors and `"none"`/unknown to
  gray.

## Scope boundaries (YAGNI)

- **No master-list management** (create/rename/delete categories) — done in
  Outlook.
- **No bulk multi-message assign**, no per-category message counts.
- **No threaded-view category filtering** — an active filter shows a flat
  matching list.
- **Colors are best-effort** terminal approximations of the 25 presets.
