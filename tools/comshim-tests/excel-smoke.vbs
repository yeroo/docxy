' Late-bound Excel automation smoke test (canonical IDispatch client).
' Runs UNCHANGED against real Excel or the xlcomshim shim. Creates a small
' workbook with values and a live formula, then SaveAs the path in arg 0.
'   cscript //nologo excel-smoke.vbs C:\path\out.xlsx
Dim out
out = WScript.Arguments(0)

Dim x, wb, ws
Set x = CreateObject("Excel.Application")
x.DisplayAlerts = False
Set wb = x.Workbooks.Add()
Set ws = wb.Worksheets(1)
ws.Name = "Report"
ws.Range("A1").Value = "Item"
ws.Range("B1").Value = "Qty"
ws.Cells(2,1).Value = "Widgets"
ws.Cells(2,2).Value = 10
ws.Cells(3,1).Value = "Gadgets"
ws.Cells(3,2).Value = 32.5
ws.Range("B4").Formula = "=SUM(B2:B3)"
WScript.Echo "B4 (pre-save) = " & ws.Range("B4").Value2   ' expect 42.5
wb.SaveAs out, 51                                          ' 51 = xlOpenXMLWorkbook (.xlsx)
wb.Close
x.Quit
WScript.Echo "wrote " & out
