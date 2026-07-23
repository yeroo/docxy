' Page breaks: two paragraphs separated by a page break -> a 2-page document.
Option Explicit
Dim app, doc, out
out = WScript.Arguments(0)
Set app = CreateObject("Word.Application")
app.Visible = False
Set doc = app.Documents.Add()
app.Selection.TypeText "Page one content."
app.Selection.InsertBreak 7        ' wdPageBreak
app.Selection.TypeText "Page two content."
doc.SaveAs2 out
doc.Close
app.Quit
WScript.Echo "OK -> " & out
