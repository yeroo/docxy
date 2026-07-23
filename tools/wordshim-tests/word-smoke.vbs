' Late-bound Word automation against the shim: create a document, type text and
' paragraphs, set a Range's text, and SaveAs a .docx. Prints OK + the path.
Option Explicit
Dim app, doc, sel, rng, out
out = WScript.Arguments(0)

Set app = CreateObject("Word.Application")
app.Visible = False
WScript.Echo "Name=" & app.Name & " Version=" & app.Version

Set doc = app.Documents.Add()
Set sel = app.Selection

sel.TypeText "Docxy Word Shim"
sel.TypeParagraph
sel.TypeText "First body paragraph."
sel.TypeParagraph
sel.TypeText "Second body paragraph."

' Append via the document Range too.
Set rng = doc.Content
rng.InsertParagraphAfter
rng.InsertAfter "Appended via Range.InsertAfter."

doc.SaveAs2 out
doc.Close
app.Quit

WScript.Echo "OK -> " & out
