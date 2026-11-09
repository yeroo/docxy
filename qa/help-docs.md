# Help tab: reading a manual offline (#1855) — manual case

What the automated tests cannot reach: a real build with a manual installed in a
person's own config directory, read with no network, with real mouse clicks,
scrolling and the macOS chords.

`uiharness/cases/help-docs.uit` drives the Help tab on the helpdocs word fixture
(installed into its sandbox by its `manual` line), and the suite's unit tests
cover the layout, the history, the search and the F1 routing. This case covers
what only a person at a real build can see.

**Setup:** build the suite (`cargo build --release --manifest-path suite/Cargo.toml`).
Copy `helpdocs/tests/fixture/word` to `<config root>/docxy/manual/word` (the config
root is `$DOCXY_CONFIG_DIR` when set, else `~/.config` on Linux,
`~/Library/Application Support` on macOS, `%APPDATA%` on Windows). Turn the
network off (unplug, airplane mode, or `nmcli networking off`).

None of these cases has been mutation-proven yet. The **Fails when** lines name the
regression each one is meant to catch.

## F1 opens the page for where you are

**Guards:** #1855 requires F1 to open the manual inside the suite, on the page of
the ribbon tab shown, or of the open dialog.

**Steps:**
1. Start the suite with a document. On the Home tab press **F1**.
2. Go back to the document tab, click **Insert**, press **F1**.
3. On the document, open Insert › Table › Insert Table..., press **F1**. Go back to
   the document tab.
4. On the Help tab press **F1**.

**Expect:**
- Step 1 opens a tab titled `Help: Home tab` right after the document, on the
  Home tab page, with the contents on the left and Home tab highlighted. The
  title bar's label reads `help`; the ribbon shows File only.
- Step 2 reuses that tab (no second Help tab) and shows the Insert tab page.
- Step 3 shows the Tables page; back on the document, Insert Table is still open.
- Step 4 shows the manual's first page, Welcome to docxy.

**Fails when:** F1 does nothing, opens a second Help tab, opens a browser, closes
the dialog, or lands on the wrong page.

## F1 on the command under the pointer

**Guards:** #1869 requires F1 with the pointer resting on a ribbon command to open
that command's page, and the pointer leaving the window to end it.

**Steps:**
1. On the document's Home tab rest the pointer on **Bold** (no click) and press
   **F1**. Go back to the document tab.
2. Rest the pointer on Insert › **Symbol** and press **F1**. Go back.
3. Rest the pointer on Home › **Bold**, then move it straight up out of the
   window, across the title bar's edge onto the desktop, and press **F1** (on
   Windows and Linux click nothing: the window keeps the keyboard).

**Expect:**
- Step 1 shows the Font formatting page (`home-font`).
- Step 2 shows the Insert tab page: Symbol has no page of its own.
- Step 3 shows the Home tab page, not Font formatting: a pointer outside the
  window rests on no command.

**Fails when:** F1 over a command opens its tab's page, or F1 with the pointer
outside the window still opens the page of the command it last rested on (the
Windows check: gpui sends no `MouseExited` there).

## Search, links and history work with no network

**Guards:** #1855 requires full-text search, links between pages, back/forward
and offline reading.

**Steps:**
1. On the Help tab click the search box and type `strikethrough`.
2. Press **Enter**.
3. Click **Welcome to docxy** in the contents, then the `keyboard shortcuts` link.
4. Press **Alt+←** (⌘[ on macOS), then **Alt+→** (⌘]); then the Back and Forward
   buttons.
5. Click the `GitHub` link.

**Expect:**
- Step 1 replaces the contents with one hit, Font group, with its snippet, as you type.
- Step 2 opens the Font group page.
- Step 3 opens the Keyboard page scrolled so the Shortcuts heading is at the top,
  its chords drawn as key caps (`Ctrl` `+` `Shift` `+` `L`).
- Step 4 walks back to Welcome and forward again; a button greys when there is
  nowhere to go.
- Step 5 asks the browser to open the page (it fails offline); the Help tab stays
  where it was.

**Fails when:** search needs the network or finds nothing, a link does nothing,
the anchor does not scroll, or Back/Forward lose the place.

## Light and dark, pictures and tables

**Guards:** #1855 requires tables, images, code, key chords and both themes.

**Steps:**
1. Open the Tables page, then Pictures, then Home tab.
2. File › Settings: switch the theme to Dark (and back to Light).
3. Press **Ctrl+S** on the Help tab, then close it with its chip's ×. Quit the suite
   and start it again.

**Expect:**
- The table has a shaded header row and its columns are left, centred and right
  aligned, and its code block (under the table) uses a monospaced font on a
  panel. The Pictures page draws its (tiny) picture. On the Home tab page the
  lists nest with their own bullets, and `Heading 1` (the last line) is inline
  code: monospaced, on a panel.
- Every part follows the theme: no white page left in Dark.
- Ctrl+S says Help pages are not saved. After the restart no Help tab comes back.

**Fails when:** a table is ragged, a picture is missing, a part stays light in Dark,
Ctrl+S writes a file, or a Help tab is restored.

## No manual installed

**Steps:** remove `<config root>/docxy/manual/word`, restart the suite, press **F1**.

**Expect:** the status line says "Help: the documentation is coming soon", as
before #1855 (this build's `docs/manual/` holds no manual of its own).

**Fails when:** F1 opens an empty tab or an error.
