# File screen (Backstage) — manual case

Run by a person on the desktop suite, in light and dark. Automated twin:
`uiharness/cases/backstage-scroll.uit`.

## The File screen scrolls in a short window

**Guards:** #1028 — content below the window's bottom edge was unreachable on
the File screen.

**Steps:**
1. Open `assets/sample.docx` in the suite and shrink the window to about
   1000×500 (or less).
2. Click File. On the default (Open) page turn the mouse wheel down: the
   page scrolls and a scrollbar shows on the right. Scroll to the end and
   check the last line (the Trusted Documents text) is fully visible.
3. Press PageDown, PageUp, End and Home: the page scrolls by a screenful, to
   the bottom and back to the top.
4. Click Info, then New: each page starts at the top, even after you scrolled
   the previous one. Scroll the Open page down, close File (Back or Esc) and
   click File again: the default page is back at the top. (Open… in the rail
   opens a file picker, not a page.)
5. Make the window as short as it goes (it stops at 420 px): the rail's
   last item, Close, is still visible, or the rail scrolls to it.
6. Switch the theme with the title bar's button and repeat step 2. No
   horizontal scrollbar appears at normal widths.

**Fails when:** content below the fold is unreachable, a page opens already
scrolled, or a horizontal scrollbar appears.
