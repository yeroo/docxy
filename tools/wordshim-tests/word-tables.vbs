' Word table automation against the shim: create a 2x3 table and fill cells via
' Table.Cell(r,c).Range.Text, then SaveAs. Prints OK.
Option Explicit
Dim app, doc, tbl, out
out = WScript.Arguments(0)

Set app = CreateObject("Word.Application")
app.Visible = False
Set doc = app.Documents.Add()

app.Selection.TypeText "Report with a table:"
app.Selection.TypeParagraph

Set tbl = doc.Tables.Add(app.Selection.Range, 2, 3)
tbl.Cell(1, 1).Range.Text = "Name"
tbl.Cell(1, 2).Range.Text = "Qty"
tbl.Cell(1, 3).Range.Text = "Price"
tbl.Cell(2, 1).Range.Text = "Widget"
tbl.Cell(2, 2).Range.Text = "10"
tbl.Cell(2, 3).Range.Text = "3.50"

WScript.Echo "Tables.Count = " & doc.Tables.Count
doc.SaveAs2 out
doc.Close
app.Quit
WScript.Echo "OK -> " & out
