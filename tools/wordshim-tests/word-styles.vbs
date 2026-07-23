' Heading styles: set Selection.Style then type; real Word should show the
' paragraphs as headings (style name + heading formatting).
Option Explicit
Dim app, doc, out
out = WScript.Arguments(0)
Set app = CreateObject("Word.Application")
app.Visible = False
Set doc = app.Documents.Add()
app.Selection.Style = "Heading 1"
app.Selection.TypeText "Chapter One"
app.Selection.TypeParagraph
app.Selection.Style = -3            ' wdStyleHeading2
app.Selection.TypeText "A Section"
app.Selection.TypeParagraph
app.Selection.Style = "Normal"
app.Selection.TypeText "Ordinary body text."
doc.SaveAs2 out
doc.Close
app.Quit
WScript.Echo "OK -> " & out
