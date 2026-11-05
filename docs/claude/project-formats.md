# Project file formats

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project file reading and writing: format plugins, MPX, `.mpp` custom fields. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Format plugins

- A format plugin is a `yppx_*` shared library (`.dll`/`.so`/`.dylib`)
  implementing `projcore::plugin_abi` (C ABI, MSPDI XML text across it) that
  adds project file types. `projcore::formats::Registry` holds the built-in
  `.yppx`, `.xml` and `.mpx` (read-only, #309), then the host's
  `mppread::project::MppImport` (read-only `.mpp`), then the plugins; a later registration replaces an extension's
  reader and/or writer in place. The loader is `projcore::formats::plugin`,
  behind the `plugins` feature (it adds `libloading`), which only native hosts
  enable; projcore's default and wasm builds stay dependency-free.
- `yppxy` loads them once at start from `<config>/yppxy/plugins`, then
  `$YPPX_PLUGIN_DIR` (`yppxy/src/formats.rs`). The suite loads them into one
  process-wide registry (`suite/docxy/src/project_formats.rs`, `global()`,
  first asked in `main`) from `<config root>/docxy/plugins` (`DOCXY_CONFIG_DIR`
  when set), then `$YPPX_PLUGIN_DIR`. That registry decides `is_project_path`,
  the Open dialog's lists (`open_extensions()`), every read and write, the
  Save As dialog's types (`save_types(path)`, the proposed type first, then
  registry order), the harness's `save-as` and `save-types`, and control
  `proj.save`. Unit tests load none; `project_formats::tests::Override` swaps
  in a stand-in per thread.
- A library may also export `yppx_plugin_read_ex` / `yppx_plugin_write_ex`
  (#1702; independent, optional, ABI still 1) to carry the plan's
  `Formatting`, which MSPDI does not: write_ex gets the `formatting.xml` text
  beside the MSPDI (NULL/0 when the plan has neither formatting nor rich
  notes; its `<note>` children are the rich notes, #1801, which read_ex must
  return for them to survive) and is called instead of
  `yppx_plugin_write`; read_ex returns it in a second buffer (empty leaves the
  default; non-UTF-8 or unparsable fails the read with the plugin's name).
  `.yppx` keeps it in `formatting.xml` as before. Hosts need nothing: they pass
  the whole `Project` to `Registry::write`. The fixture appends a hex
  `fixture-formatting` trailer (`projcore/tests/plugin_fixture/src/fmt.rs`);
  the no-`_ex` fallback is unit-tested in `plugin.rs` with stub fns.
- A format plugin may be the default save type: `"default": true` on a
  formats JSON entry (`FormatDecl::default`, `ProjectFormat::is_default`;
  ABI still 1). Save As proposes `Registry::default_save_extension(opened)`:
  the plan's own type when something writes it, else the last registered
  default that still writes, else `.yppx`. Both hosts put it first
  (`save_types_default_first`) and in the suggested name (`Project1.<ext>`,
  `plan.<ext>`); Save in place and an extensionless `save_target` are
  unchanged. The harness's `save-types` reports `default`, `proj.path`
  `default_save_ext`; the fixture declares `.fakeproj` the default.
- Imported-status rule: a Project type nothing writes is an import (status
  "…, imported from .<ext>; Save As .yppx or MSPDI to keep edits", Save goes
  to Save As, `proj.path`/`tab-list` `imported: true`). With a plugin that
  writes `.mpp`, an `.mpp` tab saves in place. The opened file (re-read for a
  sidecar restore, refreshed by `proj.reload`) is `ProjectView::source`, passed
  to a plugin's save of the same extension only.
- A plugin that fails to load is skipped: stderr and the crash log at start,
  the first window's status line, never a dialog. The public build ships no
  plugin; without one every list, status and refusal is the built-ins'.
- The fixture is `projcore/tests/plugin_fixture` (lib `yppx_fixture`, a fake
  `.fakeproj` type). `uiharness/cases/project-plugin.uit` starts with
  `plugin yppx_fixture`: the runner copies it from beside uiharness into the
  sandbox's `docxy/plugins` before launch (build with `cargo build --release
  -p uiharness -p plugin_fixture`; `ui-sweep-linux` does). Harness instances
  never inherit `$YPPX_PLUGIN_DIR`.

## MPX import (#309)

- `projcore::mpx::read_mpx` reads MPX (dependency-free; UTF-8, else
  Windows-1252, or 1251 when the header's code page field is `RU`; the delimiter is the character after `MPX` in the header,
  record 10 is only the number format). `formats::Mpx` (`.mpx`, label "MPX")
  is a built-in of `Registry::new`, read-only, so every host lists it in
  Open and treats an opened `.mpx` as an import (Save goes to Save As) by
  the existing rule, with no host code naming it. `Registry::read` reads a
  file whose extension nothing claims as MPX when it starts `MPX,` or `MPX;`
  (`mpx::is_mpx`, after an optional BOM), else as MSPDI as before; the
  suite's `Formats::read` refuses such an extension before that and projctl
  never calls `Registry::read`, so only yppxy sniffs (its CLI and
  `proj.open`/`proj.reload`). The MSPDI reader (`formats::Mspdi`) reads
  MPX bytes as MPX too (#1805), sniffed before its UTF-8 check, so an MPX
  file named `.xml` opens; it is the `.xml` type, not an import, so Save
  rewrites it as MSPDI under its own name, by design.
- Rows are read by the field ids of the 41/61 record before them; tasks are
  ordered by ID (a row with no ID keeps its place), a row with no name and
  no outline level is blank, a stated level 0 is kept (Project's umbrella
  row, the project summary when it is UID 0), links name task IDs and a
  lag other than `%` is read by the duration rules, not the cell grammar. Dates are whole days: a
  constraint, actual, stop or stored date lands on `editor::day_start` /
  `day_end` of the task calendar by its kind. Start/Finish are only the
  stored dates, never constraints. Enumerations (Priority as MPXJ maps
  them, Medium = 500), Yes/No, link types and duration and rate units are
  the words of one locale per file (`mpx/locale.rs`, #1805: MPXJ's
  `LocaleData` bundles for en de fr it pt sv es ru, units by meaning, never
  by the bundle's index; Chinese is English): `RU` in the header is
  Russian, else the word, duration and Predecessors fields (by their 61/41
  ids as MPXJ types them, record 75 by place; never a name or other free
  text) score a locale word, a unit English does not read or a link type
  English lacks; ties go to English, then table order (German before
  Swedish on `t` alone, a known limit). A word the locale lacks is read as
  English, as is a unit whose locale suffix leaves no number (German
  `2em`); U+00AD is ignored (MPXJ's Spanish `Sí`). Units are fractions (`1` = 100%), as MPXJ writes
  and reads them; durations take MPXJ's `mon`/`emon` and `y`/`ey` beside
  Project's `mo`/`emo`, the longest suffix first; dates may spell the month
  in English (formats 2, 3, 5, 10: `March 05 2026`, `05 Mar '26`), and a
  weekday before a date (a word, maybe ending in `.`, that is not an
  English month or comes before a numeric date: Spanish `mar 03/03/26`) is
  skipped; localised month names are not read; a note's
  U+007F is a line break.
- MPX has no task calendar, Manual, Active or Honor Constraints field, and
  constraint dates have no time: `projcore/tests/mpx_corpus.rs` compares
  `corpus/mpx` (MPXJ 16.8.0's MPX of `corpus/mspdi`, three date orders)
  with the twins and lists those exclusions; `every_exclusion_is_needed`
  drops a stale one.

## Custom fields from .mpp (#292)

- `mppread/src/customfields.rs` reads task custom fields of the current
  layout only (MPP12 and MPP9 decode none). A value is a task VarMeta entry
  whose key is the low word of its MSPDI FieldID (`0x0B40_0000 | key`):
  Text1 `0x0033` (UTF-16 + NUL), Start1 `0x0034`, Finish1 `0x0035`, Date1
  `0x0109` (the four-byte task timestamp), Number1 `0x0057` (`f64`, written
  `42`). Only keys in `FIELDS` are read (UID 33 also has `0x00a4`..`0x00aa`
  holding 0.0, which the XML omits); a value that does not decode is left
  out, never an error. Values are kept ascending by FieldID (Project's own
  value order is neither that nor VarMeta's). UID 0's values are decoded
  with its row but dropped by the import with the project summary row.
- Definitions come from `TBkndTask/Props` (a `props::entries` stream): key
  `0x0440_0001`, 36 bytes not understood, a `u32` count, a `u32` length
  (`count * 88`), then 88-byte records ascending by FieldID (FieldID +0,
  type +4, SecondaryPID +8, Guid +20 in Windows layout). SecondaryGuid is
  derived (the Guid's first 28 characters plus the SecondaryPID in hex: six
  of six in the corpus); FieldName comes from `FIELDS`, so a record of a
  field not there is skipped. A block of any other shape gives no
  definitions. `TBkndAssn/Props` (assignment definitions?) is not read.
- Flag1's value is not read: UID 33 of snapshots 23..46 sets two Fixed2Meta
  bits (byte 0x1f bit 7, byte 0x3d bit 7) and also carries field 188744339,
  which has no definition, so nothing tells which bit is Flag1.
  `custom_field_oracles` lists both FieldIDs as exclusions and asserts each is
  needed; `custom_fields_round_trip_through_mspdi` covers `.xml` and `.yppx`
  Save As (one `write_mspdi`).
