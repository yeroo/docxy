' Graceful-degradation smoke: drive the shim late-bound (as Petrel would) but
' lean HEAVILY on members the shim does NOT model -- unknown Application settings,
' Range formatting, collection walks, and method calls. None of them must fault:
' the shim has to swallow puts, hand back a do-nothing object for gets, and still
' produce a valid workbook. Prints OK and the saved path on success.
Option Explicit
Dim app, wb, ws, rng, out
out = WScript.Arguments(0)

Set app = CreateObject("Excel.Application")
app.Visible = False
app.DisplayAlerts = False

' --- unmodeled Application settings (all should be swallowed) ---
app.ScreenUpdating = False
app.EnableEvents = False
app.Cursor = 1
app.StatusBar = "Exporting..."
app.Calculation = -4135          ' xlCalculationManual
app.Interactive = False

Set wb = app.Workbooks.Add()
Set ws = wb.Worksheets(1)
ws.Name = "Report"

' --- real content (must be preserved) ---
ws.Range("A1").Value = "Item"
ws.Range("B1").Value = "Qty"
ws.Cells(2, 1).Value = "Widgets"
ws.Cells(2, 2).Value = 10
ws.Cells(3, 1).Value = "Gadgets"
ws.Cells(3, 2).Value = 32.5
ws.Range("B4").Formula = "=SUM(B2:B3)"

' --- unmodeled Range formatting (swallowed, output still valid) ---
ws.Range("A1:B1").Font.Bold = True
ws.Range("A1:B1").Font.Size = 12
ws.Range("A1:B1").Font.Name = "Calibri"
ws.Range("A1:B1").Interior.Color = 15773696
ws.Range("A1:B1").HorizontalAlignment = -4108
ws.Columns("A:B").AutoFit
ws.Range("B2:B4").NumberFormat = "#,##0.00"
ws.Range("A1").Borders.LineStyle = 1

' --- unmodeled collection walks / method calls (do-nothing object) ---
ws.PageSetup.Orientation = 2
wb.Names.Add "MyRange", ws.Range("A1:B4")
ws.Protect "pw"
ws.Unprotect "pw"

wb.SaveAs out, 51
wb.Close False
app.Quit

WScript.Echo "OK -> " & out
