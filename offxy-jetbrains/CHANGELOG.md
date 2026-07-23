# Changelog

## 0.1.0 (unreleased)

Initial release.

- Native `.xlsx` editor: virtualized grid over gridwasm's windowed viewport
  protocol (Chicory, pure JVM) — values/formulas with live recalc, formula
  bar, formatting toolbar (bold/italic/align/decimals/autosum), insert/
  delete rows/columns with reference rewriting, sheet strip, TSV clipboard,
  engine-stack undo (one transaction = one Ctrl+Z), lossless save, agent
  control surface (`xlsxy-jetbrains-*`).

- Native `.docx` editor for IntelliJ-platform IDEs (2024.2+): the docxy
  engine (`docxwasm.wasm` on Chicory, pure JVM) rendered in a real IntelliJ
  editor over a live editable Document — native typing latency, engine
  reconciliation, guarded decoration regions.
- Styles as editor highlighters (theme-aware ANSI palette), images inline
  (PNG/JPEG/GIF/BMP; placeholder boxes for WMF/EMF/SVG).
- Platform find, native text undo, snapshot undo for formatting commands,
  Save All / close-save, external-change reload, empty-file create flow.
- Formatting toolbar; markdown ⇄ docx conversion actions; replace-all.
- Agent control surface: tabs advertise as `docxy-jetbrains-<name>-<n>` in
  docxy's ctl dir (ctlcore wire protocol); host verbs served today, engine
  doc verbs ready for the `docx_ctl` artifact.
