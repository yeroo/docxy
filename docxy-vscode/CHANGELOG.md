# Changelog

## 0.2.0

Initial release — open and edit Word `.docx` in a VS Code editor tab.

- Faithful monospace-grid rendering (runs, headings, lists, tables, links,
  embedded images) at the editor's font/size, honoring the color theme.
- Editing: typing, navigation, selection, click/drag, copy/cut/paste.
- No-ribbon formatting toolbar + `Docxy: …` command palette (bold/italic/
  underline/strike, headings, bulleted & numbered lists, alignment, font size).
- Find (VS Code find widget) and Replace (`Docxy: Replace…`).
- Native dirty state, undo/redo, Save, Save As, and hot-exit backup.
- **Lossless save** — edits the real OOXML model; unmodeled parts are preserved.

Powered by a WebAssembly build of the dependency-free `docxcore` engine.
