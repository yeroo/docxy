# Editable HTML (`*.docx.html`) — manual cases

What Playwright (`webapp/e2e/`) cannot reach: the page next to the real
desktop suite, the native Save dialog, and a real input method. Run by a person.

There is no `qa/product.md` adapter for docxy's editable HTML yet. These cases
drive a browser by hand, so the no-global-input rules for automated runs don't
apply. Still use a scratch copy of the document, never a file you care about.

**Setup for every case:** build `cargo build -p docxy --features html-export`,
copy `assets/sample.docx` into a scratch folder, and run
`target/debug/docxy <scratch>/sample.docx --html <scratch>/sample.docx.html`.
Confirm the file is fresh: Backstage › Info › Exported shows the current time.

None of these cases has been mutation-proven yet (see ui-qa). The **Fails when**
lines name the regression each one is meant to catch.

## The page looks like the desktop suite, in light and dark

**Guards:** #164 requires the suite's window and ribbon, not the old grid webview.

**Steps:**
1. Open `sample.docx` in the desktop suite (`suite.exe`). Open
   `sample.docx.html` in Chromium, side by side, both at 100%.
2. Compare: title bar (wordmark, Undo/Redo, document chip, theme button),
   ribbon tabs (File in the accent colour), every Home group and its buttons,
   the Styles gallery, the grey canvas with the white page, and the status bar.
3. Switch both to Dark with the theme button, then compare again.
4. In each app, put the caret in the table. Check that the Table tab appears.

**Expect:** the same tabs, groups, buttons, icons and order. The same Auto → Light → Dark
cycle, with the page staying white on a dark canvas. The Table tab shows only
while the caret is in the table. Differences in text rendering are fine.
Missing or reordered controls are a failure.

**Fails when:** `htmlbundle/web/ribbon-docx.json` is edited by hand instead of
regenerated, or `app.js` `control()` stops rendering a control kind.

## Chromium saves in place and reuses the file on the next Ctrl+S

**Guards:** #164 requires Chromium to save the same `sample.docx.html` in place.

**Steps:**
1. Open `sample.docx.html` in Chrome or Edge. Type `one` at the end of the document.
2. Press Ctrl+S. In the native Save dialog, pick the same file and confirm the
   overwrite.
3. Type `two` and press Ctrl+S again.
4. Close the tab. Reopen the file.
5. Run `docxy <scratch>/sample.docx.html --md out.md`.

**Expect:**
- Step 2 shows exactly one dialog. Step 3 shows none, and the chip's `•` clears.
- The reopened page shows `one` and `two`.
- `out.md` contains both.
- There is no `sample (1).docx.html` download in the Downloads folder.

**Fails when:** `save()` drops `S.fileHandle`, or it downloads even though
`showSaveFilePicker` exists.

## Firefox falls back to a download with the same name

**Steps:**
1. Open the file in Firefox. Type `ff` and press Ctrl+S.

**Expect:** Firefox downloads a file named `sample.docx.html` (Firefox may add
` (1)` if one exists). The downloaded file opens with `ff` in it, and
`docxy <download> --docx back.docx` succeeds.

**Fails when:** the fallback names the file after `sourceName` without
`.html`, or skips the rebuild.

## A real CJK IME inserts the composed text once

**Guards:** composition can't be cancelled in `beforeinput`. The page lets the
IME draw, then discards that DOM and inserts the committed text.

**Steps:**
1. Chromium, with the Microsoft Japanese IME on: click at the end of the first
   body paragraph and type `kana`, then Space and Enter to commit `かな` (or
   any kanji).
2. Repeat in Firefox.
3. In each browser, save and convert with `docxy … --md`.

**Expect:** the committed text appears once, where the caret was. There's no
leftover romaji or duplicate, and the Markdown has it once.

**Fails when:** `onCompositionEnd` stops re-rendering before inserting, or
`onBeforeInput` stops skipping `isComposing` events.

## Closing with unsaved edits asks first

**Steps:**
1. Type one character and close the tab.
2. Save, then close again.

**Expect:** step 1 shows the browser's "Leave site?" prompt. After the save
there's no prompt.

**Fails when:** the `beforeunload` handler stops checking `S.dirty`, or save
stops clearing it.
