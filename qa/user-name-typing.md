# User name and Initials typing — manual cases

What the scripted harness cannot reach: a person's real keyboard and mouse in
the desktop suite's window. `uiharness/cases/user-name.uit` covers the same
paths with synthetic events dispatched through gpui's input path; run these
once on each OS after a change to the window's key handling. Use a scratch
config (`DOCXY_CONFIG_DIR` or a fresh profile), never your own.

**Setup for every case:** `cargo build -p docxy`, run the suite, open
`assets/sample.docx`.

## Typing reaches a dialog opened from Backstage

**Guards:** #1027 (File › Settings › User name… took no keys from Backstage).

**Steps:**
1. File › scroll to Settings › click **User name...**.
2. Without clicking, press Ctrl+A and type `Jane Doe`. Press Tab, Ctrl+A, type `JD`.
3. Drills, in the first field (finish each by retyping the text with Ctrl+A):
   type `Jane`, press Home, Right, Delete; press End, Left, then type `x`;
   press Shift+Home, then Backspace.
4. Click between two letters of a field and type; paste text with Ctrl+V over a
   Ctrl+A selection. Then Ctrl+A in each field and set `Jane Doe` and `JD` again.
5. Press OK. Close File, select a word, Review › New Comment, type `hi`, Enter.

**Expect:** the first field has the caret as soon as the dialog opens; every
key above edits the focused field at the caret; a click places the caret under
the pointer; OK keeps the values and the comment's author and initials are
`Jane Doe` and `JD`. Escape instead of OK keeps the old values. The same holds
after closing every document (the Start page).

**Fails when:** typed keys don't reach a dialog opened from Backstage, i.e. the
Backstage root in `render` (`suite/docxy/src/main.rs`) loses `key_routing`, or
a dialog field stops taking the caret.
