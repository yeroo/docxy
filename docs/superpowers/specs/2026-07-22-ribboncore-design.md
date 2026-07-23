# ribboncore — one shared ribbon for docxy / xlsxy / yppxy / lookxy

**Date:** 2026-07-22
**Status:** design approved (shared crate; drop the hint bar; lookxy's look is canonical)

## Goal

The four apps each carry a near-identical copy of the ribbon (~3,460 lines
total; ~90% shared machinery). Extract that machinery into one `ribboncore`
crate so a change lands in every app at once. Each app keeps only its own
command set, its tab/button data, its accent colour, and its command dispatch.

## Canonical look (all apps)

lookxy's current ribbon: a **closed box** drawn entirely in the app **accent**
colour (tab names, borders, group titles, button glyphs), the **selected tab
inverted** (black on accent), and **no hint bar** (the black-on-yellow strip is
removed everywhere). Collapsed = the 1-row tab strip; expanded = the 6-line
bordered body (top border, two button rows, mid divider, group titles, bottom
border). `EXPANDED_H = 7`.

## `ribboncore` API (generic over the command type `A: Copy + PartialEq`)

- Data model: `Button<A> { glyph: &'static str, width, act: A, hint }`,
  `Seg<A>` (`Btn`/`Gap`), `Group<A> { title, width, rows: [Vec<Seg<A>>; 2] }`,
  `Placed<A>`, `Focus` (`None`/`Tab(usize)`/`Button(usize)`), `Hit<A>`
  (`Tab`/`Button(A)`/`Outside`), `Dir`. Free helpers `btn(...)`, `gap(...)`.
- `Ribbon<A>` fields: `tabs: Vec<&'static str>`, `active`, `tab_groups:
  Vec<Vec<Group<A>>>`, `placed`, `tab_cols`, `active_toggles: Vec<A>`,
  `glyph_overrides: Vec<(A, &'static str)>`, `accent: Color`.
- Constructor `Ribbon::new(tabs, tab_groups, active, accent)` (lays out).
- Mutators: `set_active(i)`, `set_tab_groups(i, groups)` (re-lays out if `i`
  active — powers docxy's markdown-context swap and lookxy's mail/calendar Home
  swap), `set_toggles(Vec<A>)`, `set_glyph_override(act, glyph)` (powers
  docxy's ☀/☾ Dark-Mode glyph).
- Queries: `active_tab`, `tab_label(i)`, `tab_has_body(i)`, `has_act(act)`,
  `button_count`, `width`, `focus_act(focus)`, `focus_hint(focus)`.
- Mouse: `hit(x, y, expanded) -> Hit<A>`.
- Keyboard: `enter_body()`, `nav(focus, dir) -> Focus`.
- Render: `render_tabs(focus)`, `render_tabs_as(active_tab)`,
  `render_body(focus)` — all in the accent, closed box, no hint. `EXPANDED_H`
  const. (No `render_hint` — dropped.)

The glyph override list is consulted in `render_body`: a button whose `act`
matches an override draws the override glyph instead of `Button::glyph`.

## Per-app wrapper

Each app keeps a thin `ribbon.rs`:

- `pub enum Act { … }` — the app's commands (unchanged).
- `pub struct Ribbon(ribboncore::Ribbon<Act>)` with `Deref`/`DerefMut` to the
  inner core, so every existing call site (`ribbon.render_tabs(…)`,
  `ribbon.hit(…)`, `ribbon.set_toggles(…)`, …) keeps working unchanged.
- App-specific **inherent** methods on the wrapper: the constructor
  (`home()`/`new()` building the tabs/groups + accent), and the contextual
  helpers (`set_markdown`/`set_light_page` for docxy, `set_home_context` for
  lookxy) — each implemented via the core's `set_tab_groups`/`set_glyph_override`.
- The **dispatch** (`Act` → the app's editor/App method) stays in the app,
  untouched.

Accent per app: **lookxy cyan · docxy light blue · xlsxy green · yppxy yellow**.

## Migration order & safety

1. Create `ribboncore` (port lookxy's current machinery, generic; unit-test it).
2. Migrate **lookxy** onto it (reference look) — its ribbon tests must pass.
3. Migrate **docxy** (markdown context + dark-mode glyph + styles) — its ribbon
   tests must pass.
4. Migrate **xlsxy**, then **yppxy** — their ribbon tests must pass.

After each app: that app's tests green, `clippy --all-targets -D warnings`
clean, `fmt` clean. The full workspace must be green before the final push.

## Out of scope

- Changing any app's command set, button inventory, or dispatch behaviour.
- The backstage (already per-app; only lookxy's was reworked).
- Any new ribbon features — this is a consolidation, not a feature.
