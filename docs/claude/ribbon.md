# Ribbon

Notes moved from `CLAUDE.md` (#1903). Read them before changing the ribbon's fit, group launchers and swatch galleries. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Contributed group launchers (#1669)

- A manifest group may carry `"launcher":{"id","tip","keytip"}`
  (`uiplugin::Launcher`; an object without `id` fails the manifest naming the
  group, any other value is none; `Manifest::json` writes it after
  `priority`). `plugin_ui::extend_tab_with` sets `ribbonspec::Group::launcher`
  to `act_of(id)` with `launcher_tip`/`launcher_key` (`&'static str`, empty
  for the built-ins: the tip falls back to "More options"), on the new group
  or on the group it joins, replacing that one's launcher, even when every
  control replaced a built-in; a new group with no control is still not
  made. `render_group` draws the tip, a `ribbon-launch:<title>` probe (flyout
  prefix in a flyout copy) and, in place only, the KeyTip badge;
  `tab_keytip_cmd`/`tab_keytip_starts` take `launcher_key` after the commands'
  tips (a built-in one-letter tip still shadows a two-letter one it starts).
- The harness's `ribbon-launch:<title>` takes any title and resolves it at
  measure time against the shown tab's groups, any case
  (`harness::launch_title`); `ribbon-read` adds `launcher_tip` and
  `launcher_key_tip`. The fixture's Fixture (Task) and Fixture Format (Gantt
  Chart Format, drawn in place at CI widths) groups launch `fixture.font`
  (KeyTip ZN); `project-plugin-ui.uit` clicks one and types the other.

## Ribbon fit per ribbon (#1020, #1608)

- `ribbon_fit::fit_with(specs, width, policy)`: `Policy::LabelsFirst` (the
  document's and the sheet's ribbons, Word's and Excel's way) lets groups
  drop their labels one at a time, lowest priority first, before any
  collapses; `Policy::WholeGroups` (the Project ribbon, Project's way) skips
  that pass: a group keeps icon and label until it collapses whole into one
  labelled button, lowest priority (rightmost among equals) first, only
  while the tab overflows; the issue's "rightmost first" is read as the
  tie rule, and Project 2024's own collapse order across its priorities is
  not measured. `ribbon_fit_now` picks the policy from the
  active tab's kind; `ribbon-layout` never reports `icon-only` on a
  Project tab. A toggle with no icon (a contributed one may name none)
  draws and is estimated with its label. `ribbon-read`'s group states fit
  under the same policy (`Docxy::ribbon_policy`), so they agree with
  `ribbon-layout`. `uiharness/cases/ribbon-fit.uit` pins the document and
  sheet order, `project-plugin-ui.uit` the Project one with the fixture's
  Task group at 1180 px only (the CI displays cannot show that tab in full,
  so the widening back to full groups is pinned by `ribbon-fit.uit` and the
  `ribbon_fit` unit tests) and the Gantt Chart Format tab, whose three groups
  are meant to fit at every CI width (not run on macOS), where the fixture's
  icon-less `fixture.flat` toggle keeps `estimates_ok` true only while its
  label is drawn.
- A contributed ribbon group's collapse priority (#1685): `RibbonGroup.priority`
  (JSON `priority`, 0 to 255 as the built-ins' run to 200, clamped, a
  non-number is absent); a new group without one takes the priority of the
  group (built-in or already contributed) named by `before`, else `after`,
  else 50; joining a built-in
  group changes nothing. Contributed small buttons stack three deep
  (`controls_of`) and a label over `CONTRIB_LABEL_MAX` (24 characters) is cut
  with an ellipsis at construction, its full text the tip, so the estimate
  and the renderer read the same command. A plugin swatch gallery
  (`plugin_ui::is_swatch_gallery`, one predicate for `Docxy::swatch_gallery`
  and `ribbon_fit::gallery_geom`) draws bare tiles, so its estimate has no
  well `frame`: `n * w + (n - 1) * gap`. In place it is bounded to
  `IN_PLACE_TILES` tiles plus the More button's gap and `MORE_W` (#1823), so a
  long gallery no longer collapses its group by width alone; `fit_with` keeps no state, so a group collapsed at a
  narrow width is full again at a wide one (`plugin_ui` tests pin both).
- `fit_with` ends with a back-fill (#1763): once the collapse passes fit, groups
  they took too early (a wider group's collapse can free more than the small
  ones before it) come back, highest priority first, in two rounds under
  `LabelsFirst` (labels were dropped first, so they return last): collapsed
  groups to IconOnly (Full when icon-only saves nothing; Full only under
  `WholeGroups`), then IconOnly groups to Full. It does not run when the
  chevron is needed. The result is not monotone in the width (the
  collapse is greedy); the tests that pin the collapse order read
  `fit_passes(.., false)`, the passes without it.

## Swatch gallery palette tiles and sections (#1761)

- `uiplugin::Swatch::section` (manifest `section`, a non-string is none, written
  by `Manifest::json` only when set) and `ribbonspec::GalleryItem::section`
  (`""` for every built-in gallery). `plugin_ui::swatch_item` keeps a swatch's
  `preview` when it names a palette of a loaded manifest (the tile is that
  palette's miniature), else puts `color` as `#RRGGBB`, else the text; the
  renderer asks `tile_palette(preview)` first, then `swatch_color`.
- `plugin_ui/tile.rs` is the pure part: `palette_tile_marks` (summary bracket,
  manual bar, on-track bar, link, milestone diamond in the tile body
  `tile_body()`; colours through `project::tile_color`, which takes the chart's
  `palette_entry`, `manual` falling back to `on-track`, then the built-in
  colours; proportions are by estimate, no capture of Project's tile is in
  this repository), `gallery_layout` (a header row per run of equal non-empty
  sections, every section on a new row) and `span_of`/`gallery_span` (in a
  flyout the longest section run, in place at most `IN_PLACE_TILES`, #1823: what `ribbon_fit::model_control_w` and `Docxy::swatch_gallery`
  both wrap by, so the estimate is the width drawn). Height is
  `layout_height` (tiles plus `HEADER_H` headers plus gaps), drawn only.
  A palette tile shows no label, its label is the tooltip. Sections show in
  the flyout (expanded) copy only: in place (`max_w` None) a group is one
  fixed-height strip, so `gallery_span`/`gallery_rows` give one row with no
  headers: the `in_place_window` strip (#1823); a header is clipped to the
  gallery's width.
- `ribbon-read` adds `section` and, on a palette tile, `palette` and `marks`.
  The fixture's `fixture.styles` gallery (group "Fixture Styles", last on the
  View tab) has two sections; `project-plugin-ui.uit` reads it and shoots it
  (a `shot` is evidence, not a comparison with the plugin repo's capture).

- An in-place swatch gallery is bounded (#1823): it shows at most
  `plugin_ui::IN_PLACE_TILES` (6, an estimate: no capture of Project's) tiles,
  `in_place_window` (the first six, or the six ending on the checked swatch;
  `gallery_rows(.., None, .., checked)` is that one strip),
  and, when it has more, a `MORE_W` (16 px) More button after a gap (probe
  `gallery-more-<id>`) whose press is `flyout_toggle(FlyoutTarget::Gallery)`
  (a second press shuts it), the whole gallery
  with its sections. `gallery_span(.., None)` and `more_w` are what
  `ribbon_fit::model_control_w` and `swatch_gallery` both read, so the
  estimate is the width drawn. A flyout copy (a collapsed group's, `max_w`
  Some) wraps every tile and has no More button. No scroll arrows: the
  flyout shows the rest. The Styles and Table Styles wells are unchanged.
  `ribbon-read` adds `shown`, `first_shown` and `more` (the model's window)
  and `drawn_shown`, `drawn_first` and `drawn_more` (what the last frame drew
  in place, `Docxy::drawn_galleries`; null when not drawn in place) to each
  `galleries` entry; `gallery-more:<id>` is the More button's region. The fixture's
  `fixture.wide` (nine swatches, group "Fixture Wide" on the Report tab,
  which has room) is the bounded one in
  `project-plugin-ui.uit`.

- A gallery has a KeyTip (#1762): `ribbonspec::Gallery::key_tip` (`""` for every
  built-in), filled from a manifest gallery's `keytip`. With the key tips up
  `swatch_gallery` draws the badge on the one in the ribbon (not a flyout's
  copy) and a `gallery-<id>` probe. `tab_keytip_gallery` (after the command,
  split and launcher lookups; a two-letter key waits through `tab_keytip_starts`)
  opens `FlyoutTarget::Gallery(id)`: the gallery alone, wrapped and with its
  section headers, anchored under its probe, else its collapsed group's button,
  else the overflow chevron, else the ribbon body, whether its group is drawn
  in place or not (so it stays open for the same tab, `flyout_still_fits`). The
  `gallery-<id>` probe is drawn on every in-place frame; only the badge needs
  the KeyTips up. A minimised or hidden ribbon body opens nothing and says so. A gallery with no tiles opens nothing.
  `ribbon-read` reports `key_tip` on each `galleries` entry and `ribbon-flyout`
  names the open `gallery`; `ribbon_export` is unchanged. The fixture's
  `fixture.styles` is `XG` (`project-plugin-ui.uit`, not run on macOS).
