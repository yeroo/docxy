' Semantic heading styles + bullet/numbered lists.
Option Explicit
Dim app, doc, out
out = WScript.Arguments(0)
Set app = CreateObject("Word.Application")
app.Visible = False
Set doc = app.Documents.Add()

app.Selection.Style = "Heading 1"
app.Selection.TypeText "Report"
app.Selection.TypeParagraph
app.Selection.Style = "Normal"

app.Selection.Range.ListFormat.ApplyBulletDefault
app.Selection.TypeText "First bullet"
app.Selection.TypeParagraph
app.Selection.TypeText "Second bullet"
app.Selection.TypeParagraph
app.Selection.Range.ListFormat.RemoveNumbers

app.Selection.Range.ListFormat.ApplyNumberDefault
app.Selection.TypeText "Step one"
app.Selection.TypeParagraph
app.Selection.TypeText "Step two"

doc.SaveAs2 out
doc.Close
app.Quit
WScript.Echo "OK -> " & out
