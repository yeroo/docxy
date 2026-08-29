# DOCX mutation route inventory

This inventory is the routing contract for docxy's central protection policy.
Routes are classified by their semantic effect, not by which low-level package
parts happen to change. A route must call `App::authorize_mutation` with the
listed class before its first editor, history, package, comment, or save-state
mutation.

## Mutation classes

| Class | Meaning |
|---|---|
| Content | Changes user-visible text or inline content without changing document topology. |
| Structure | Adds, removes, reorders, splits, or merges document blocks or structural elements. |
| Formatting | Changes run, paragraph, list, section-layout, style, or other presentation properties. |
| Comment | Adds, edits, or removes review comments and their anchors. |
| Package metadata | Changes non-content package state or replaces the protected editing context through a format conversion. |

When one action has several implementation effects, its semantic class wins.
For example, applying a list is Formatting even though it may add a numbering
part, and adding a comment is Comment even though it also adds body anchors.

## Interactive routes

| Surface | Routes | Class |
|---|---|---|
| Body keys | printable characters, non-breaking space, Tab | Content |
| Body keys | Enter, Backspace, Delete | Structure |
| Clipboard | Cut | Structure |
| Clipboard | Plain/merge paste | Content |
| Clipboard | Rich paste / Paste Special Keep Source Formatting | Content and Formatting |
| History | Undo and Redo | Content; protected sessions can only contain previously authorized editor changes |
| Find/replace | replace-current and replace-all from the find bar | Content |
| Character editing | Shift+F3 case cycling | Content |
| Character formatting | bold, italic, underline, strike, subscript, superscript, grow/shrink font, clear formatting | Formatting |
| Paragraph formatting | alignment, indentation, line spacing, styles, font/color/highlight pickers, paragraph dialog, borders, horizontal-rule autoformat | Formatting |
| Paragraph ordering | Sort | Structure |
| Lists | bullets and numbering, including numbering-part creation | Formatting |
| Insert ribbon/dialogs | symbols, fields, page number, equations | Content |
| Insert ribbon/dialogs | table, page break, section break | Structure |
| Document layout | columns, hyphenation, section orientation | Formatting |
| Review | commit new comment and delete comment | Comment |
| Header/footer | typing, deletion, formatting, paste, and committing the edited part | same Content, Structure, or Formatting class as the body operation |
| Vim insert mode | typing, newline, deletion, paste | Content or Structure as above |
| Vim normal/visual mode | `x`, `d`, `c`, `D`, `dd`, paste, `o`/`O`, undo, redo | Content or Structure as above |
| File backstage | cross-format Save As that replaces the live DOCX editing context | Package metadata |

Selection, navigation, copy, find/search, comment navigation, view preferences,
inspection, same-format save, PDF/text/Markdown export, opening/reloading another
file, and creating a new document do not mutate the protected document and are
outside the authorization gate. Saving persists changes that were authorized at
the time they were made; it is not a new document edit.

## Ribbon and dialog mapping

Every implemented mutating `ribbon::Act` is covered above: Cut, Paste,
PasteSpecial, HorizontalLine, PageNumber, PageBreak, InsertTable, Columns,
Hyphenation, Bold, Italic, Underline, Strike, Subscript, Superscript, GrowFont,
ShrinkFont, ChangeCase, ClearFormatting, Bullets, Numbering, IncreaseIndent,
DecreaseIndent, FirstLineIndent, HangingIndent, Sort, ParaBorders, AlignLeft,
AlignCenter, AlignRight, Justify, ApplyStyle, NewComment, DeleteComment, and the
commits made by InsertField, InsertSymbol, InsertEquation, LineSpacing,
ParagraphDialog, StylesDialog, and font/size/color/highlight pickers. Dialog
open, navigation, and cancel paths are non-mutating.

## Control and MCP routes

| Control verb | Class |
|---|---|
| `doc.replace-range` | Structure; Markdown carrying formatting also requires Formatting |
| `doc.insert` | Structure; Markdown carrying formatting also requires Formatting |
| `doc.append` | Structure; Markdown carrying formatting also requires Formatting |
| `doc.replace-all` | Content |
| `doc.format` | Formatting |
| `doc.set-style` | Formatting |
| `doc.undo`, `doc.redo` | Content |

The Markdown forms of replace/insert/append keep their Structure requirement and
add a Formatting requirement when parsed blocks carry styles, numbering, or
direct formatting. All other current control verbs
are read, export, persistence, or document-lifecycle operations. MCP has no
independent mutation implementation: `docxy_replace_range`, `docxy_insert`,
`docxy_append`, `docxy_replace_all`, `docxy_format`, `docxy_set_style`,
`docxy_undo`, and `docxy_redo` map one-to-one to the control verbs above.

## Authorization matrix

| Protection state | Content | Structure | Formatting | Comment | Package metadata |
|---|---:|---:|---:|---:|---:|
| absent, disabled, or recommendation-only write protection | allow | allow | allow | allow | allow |
| password-backed write protection | deny | deny | deny | deny | deny |
| enforced read-only | deny | deny | deny | deny | deny |
| enforced comments-only | deny | deny | deny | allow | deny |
| enforced formatting-only | allow | allow | deny | allow | allow |
| enforced forms-only | deny | deny | deny | deny | deny |
| enforced tracked-changes-only | deny | deny | deny | deny | deny |
| enforced unknown mode | deny | deny | deny | deny | deny |

Control denials use `protection_denied:<stable_code>: <explanation>`. The stable
codes are `read_only`, `comments_only`, `formatting_locked`,
`forms_unsupported`, `tracked_changes_unsupported`, and `unsupported_mode`.
