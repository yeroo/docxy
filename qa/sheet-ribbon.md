# Spreadsheet ribbon — manual cases

What `uiharness/cases/sheet-ribbon.uit` and `ribbon-fit.uit` read as numbers,
looked at by a person on the real window. Run by a person on the desktop suite
(`suite.exe`), on a scratch copy of any workbook.

None of these cases has been mutation-proven by a person; the automated guards
(`no_ribbon_column_has_more_than_three_rows`, `ribbon-layout`) were, see #1018.
The **Fails when** lines name the regression each one is meant to catch.

## Every Home group fits the ribbon height

**Guards:** #1018 requires every ribbon group to fit the ribbon body, with at
most three small-button rows to a column.

**Steps:**
1. Open a workbook. Widen the window to at least 1400 px so nothing collapses.
2. On Home, look at each group from Clipboard to Editing: no button is cut off
   at the top or bottom, and every group title is visible under its buttons.
3. Visit Insert, Page Layout, Formulas, Data, Review, View and Help and look
   again.
4. Do the same in a Word document (Home, Insert, Design, Layout, Mailings,
   Review, View, Help) and a Project (Task, Resource, Project, View, Help).

**Expect:** every button is whole and every group title shows. Editing is an
AutoSum / Fill / Clear column, a large Sort & Filter drop-down and Find & Select.

**Fails when:** a column or stack in any ribbon definition holds a fourth row.
The sheet ribbon and a Rows stack clip it, which `ribbon-layout` measures. The
document ribbon wraps a fourth Column button into a second column instead, so
only `no_ribbon_column_has_more_than_three_rows` guards that.

## Sort & Filter is a drop-down with six items

**Guards:** #1018 requires Home > Editing to match Excel's.

**Steps:**
1. Click a cell in a column of text, then Home > Sort & Filter.
2. Read the menu, choose Sort A to Z, and check the column sorts.
3. Open it again and check Custom Sort..., Filter, Clear and Reapply.
4. Open Data > Data Tools and check Remove Duplicates is there and not on Home.

**Expect:** the menu lists Sort A to Z, Sort Z to A, Custom Sort..., Filter,
Clear, Reapply, and each runs the Data tab's matching act (Custom Sort... is Data's Sort button).

**Fails when:** the five-button column returns to Home, or the menu opens but an
item does not run.

## Page Layout and Formulas sit where Excel puts them

**Guards:** #1019 requires Excel's workbook tabs in Excel's order (APP-021,
APP-CASE-014), with the commands whose engine exists working.

**Steps:**
1. Open a workbook. Read the tab row, then press Alt, P and Alt, M.
2. On Page Layout, select A1:C5 and choose Print Area > Set Print Area; then
   Orientation > Landscape, Margins > Narrow and Print Gridlines.
3. Open Orientation and Margins again, and Width: under Scale to Fit.
4. Click Page Setup's launcher (beside its title), change Adjust to 5% and press
   OK; then 80% and OK.
5. Save, close and reopen the workbook, and open Page Setup again.
6. On Formulas, click a cell under a column of numbers and choose AutoSum's
   arrow > Average.

**Expect:** the tabs read File, Home, Insert, Page Layout, Formulas, Data,
Review, View, Help; Alt+P and Alt+M select the two new tabs. Each command in
step 2 marks the workbook changed and is one undo step; the menus tick
Landscape and Narrow, Print Gridlines shows a ticked box, and Width: ticks
Automatic. 5% is refused with "scale 5 is outside 10–400" and the dialog stays;
80% applies. After the reopen the dialog shows Landscape, the narrow margins, 80%,
gridlines and A1:C5. AutoSum writes `=AVERAGE(...)` over the run above. The
placeholders (Themes, Arrange, Name Manager, Trace Precedents, ...) do nothing.

**Fails when:** a new tab is missing or out of order, a wired command does
nothing or is not saved, or a placeholder looks like it ran.
