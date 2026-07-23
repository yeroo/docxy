' Graceful degradation + formatting: drive the shim late-bound leaning on
' unmodeled members (which must NOT fault) AND real formatting (Font.Bold/Size/
' Name/Color, Selection.Bold, ParagraphFormat.Alignment) which must land in the
' .docx. Prints OK on success.
Option Explicit
Dim app, doc, sel, out
out = WScript.Arguments(0)

Set app = CreateObject("Word.Application")
app.Visible = False

' --- unmodeled Application settings (swallowed) ---
app.ScreenUpdating = False
app.DisplayAlerts = 0
app.Options.CheckSpellingAsYouType = False   ' unmodeled object chain

Set doc = app.Documents.Add()
Set sel = app.Selection

' --- formatted heading ---
sel.Font.Bold = True
sel.Font.Size = 18
sel.Font.Name = "Arial"
sel.Font.Color = 255                 ' wdColorRed (BGR 0x0000FF)
sel.ParagraphFormat.Alignment = 1    ' wdAlignParagraphCenter
sel.TypeText "Formatted Heading"
sel.TypeParagraph

' --- back to normal body ---
sel.Font.Bold = False
sel.Font.Size = 11
sel.Font.Name = "Calibri"
sel.Font.Color = 0
sel.ParagraphFormat.Alignment = 0    ' left
sel.TypeText "Plain body paragraph."
sel.TypeParagraph
sel.Italic = True
sel.TypeText "Italic tail."

' --- unmodeled range/format members (swallowed) ---
sel.Range.HighlightColorIndex = 7
doc.Bookmarks.Add "here", sel.Range

doc.SaveAs2 out
doc.Close
app.Quit
WScript.Echo "OK -> " & out
