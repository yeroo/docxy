// Excel COM automation test from C#, exercising BOTH binding styles a .NET app
// (such as SLB Petrel) can use:
//
//   early  - Microsoft.Office.Interop.Excel PIA, strongly typed. The RCW does
//            CoCreateInstance of Excel's fixed CLSID and QueryInterfaces the
//            exact dual interfaces (_Application, _Workbook, _Worksheet, Range)
//            and calls through their vtables. This is the DEMANDING path: it
//            only works if the server exposes those interfaces (typelib/vtable),
//            not merely IDispatch.
//
//   late   - Type.GetTypeFromProgID("Excel.Application") + C# `dynamic`, which
//            binds through IDispatch (GetIDsOfNames/Invoke) at run time.
//
// Run the same binary against real Excel and against the xlcomshim shim (flip
// with office-switch.ps1) and compare. Usage:
//     ExcelInteropTest.exe early  <out.xlsx>
//     ExcelInteropTest.exe late   <out.xlsx>
//
// A clean pass prints "RESULT: OK"; a failure prints the exception type+message
// (an early-bound InvalidCastException means the server lacks the typed
// interfaces and we must ship a type library).

using System;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using Excel = Microsoft.Office.Interop.Excel;

internal static class ExcelInteropTest
{
    [DllImport("ole32.dll")]
    private static extern int CoCreateInstance(
        ref Guid clsid, IntPtr outer, uint clsctx, ref Guid iid, out IntPtr obj);

    private static int Main(string[] args)
    {
        string mode = args.Length > 0 ? args[0].ToLowerInvariant() : "early";
        string outPath = args.Length > 1
            ? args[1]
            : Path.Combine(Path.GetTempPath(), "cs-" + mode + ".xlsx");

        try
        {
            if (mode == "early") RunEarly(outPath);
            else if (mode == "earlyls") RunEarlyLocalServer(outPath);
            else if (mode == "castshim") RunCastShim(outPath);
            else if (mode == "castinproc") RunCastInproc(outPath);
            else if (mode == "castonly") RunCastOnly();
            else RunLate(outPath);
            Console.WriteLine("RESULT: OK (" + mode + ") -> " + outPath);
            return 0;
        }
        catch (Exception ex)
        {
            Console.WriteLine("RESULT: FAIL (" + mode + "): " + ex.GetType().FullName + ": " + ex.Message);
            return 1;
        }
    }

    private static void RunEarly(string outPath)
    {
        Excel.Application app = new Excel.Application();
        try
        {
            Console.WriteLine("  Name = " + app.Name + " | Version = " + app.Version);
            app.DisplayAlerts = false;
            Excel.Workbook wb = app.Workbooks.Add();
            Excel.Worksheet ws = (Excel.Worksheet)wb.Worksheets[1];
            ws.Name = "CSharpEarly";
            ((Excel.Range)ws.Range["A1"]).Value2 = "Item";
            ((Excel.Range)ws.Cells[2, 1]).Value2 = 10;
            ((Excel.Range)ws.Cells[3, 1]).Value2 = 32.5;
            ((Excel.Range)ws.Range["A4"]).Formula = "=SUM(A2:A3)";
            Console.WriteLine("  A4 = " + ((Excel.Range)ws.Range["A4"]).Value2);
            wb.SaveAs(outPath, Excel.XlFileFormat.xlOpenXMLWorkbook);
            wb.Close(false);
        }
        finally
        {
            app.Quit();
        }
    }

    // Force out-of-process activation (CLSCTX_LOCAL_SERVER), bypassing any
    // in-proc server registered for Excel's CLSID, then do the *decisive* cast to
    // the PIA's typed interface. That cast QueryInterfaces _Application
    // {000208D5}. If the shim (IDispatch only) cannot satisfy it, this throws
    // InvalidCastException -> we must ship a type library (P2). If .NET's RCW
    // works over IDispatch, early-bound needs no typelib. This mirrors what
    // happens on a machine with no Office (LocalServer32 -> the shim).
    private static void RunEarlyLocalServer(string outPath)
    {
        const uint CLSCTX_LOCAL_SERVER = 0x4;
        Guid clsid = new Guid("00024500-0000-0000-C000-000000000046");
        Guid iidUnknown = new Guid("00000000-0000-0000-C000-000000000046");
        IntPtr punk;
        int hr = CoCreateInstance(ref clsid, IntPtr.Zero, CLSCTX_LOCAL_SERVER, ref iidUnknown, out punk);
        if (hr < 0) throw new COMException("CoCreateInstance(LOCAL_SERVER) failed", hr);
        object obj = Marshal.GetObjectForIUnknown(punk);
        Marshal.Release(punk);

        // THE cast under test (QueryInterface for _Application's IID):
        Excel.Application app = (Excel.Application)obj;
        try
        {
            Console.WriteLine("  Name = " + app.Name + " | Version = " + app.Version);
            app.DisplayAlerts = false;
            Excel.Workbook wb = app.Workbooks.Add();
            Excel.Worksheet ws = (Excel.Worksheet)wb.Worksheets[1];
            ws.Name = "CSharpEarlyLS";
            ((Excel.Range)ws.Range["A1"]).Value2 = "Item";
            ((Excel.Range)ws.Cells[2, 1]).Value2 = 10;
            ((Excel.Range)ws.Cells[3, 1]).Value2 = 32.5;
            ((Excel.Range)ws.Range["A4"]).Formula = "=SUM(A2:A3)";
            Console.WriteLine("  A4 = " + ((Excel.Range)ws.Range["A4"]).Value2);
            wb.SaveAs(outPath, Excel.XlFileFormat.xlOpenXMLWorkbook);
            wb.Close(false);
        }
        finally
        {
            app.Quit();
        }
    }

    // Activate the shim by ITS OWN CLSID (no real-Excel registry conflict), then
    // do the decisive early-bound cast to the PIA's typed interface. Isolates the
    // "does .NET's RCW accept an IDispatch-only object as _Application" question
    // from all the {00024500} registration noise. InvalidCastException here means
    // early-bound clients (like Petrel, if it binds early) require a type library
    // -> we build P2 before the VDI. Success means IDispatch is enough.
    private static void RunCastShim(string outPath)
    {
        const uint CLSCTX_LOCAL_SERVER = 0x4;
        Guid clsid = new Guid("7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31"); // our shim coclass
        Guid iidUnknown = new Guid("00000000-0000-0000-C000-000000000046");
        IntPtr punk;
        int hr = CoCreateInstance(ref clsid, IntPtr.Zero, CLSCTX_LOCAL_SERVER, ref iidUnknown, out punk);
        if (hr < 0) throw new COMException("CoCreateInstance(shim CLSID) failed", hr);
        object obj = Marshal.GetObjectForIUnknown(punk);
        Marshal.Release(punk);
        Console.WriteLine("  activated shim; casting object to Excel._Application (QI {000208D5})...");

        Excel.Application app = (Excel.Application)obj; // THE cast under test
        try
        {
            Console.WriteLine("  cast OK. Name = " + app.Name + " | Version = " + app.Version);
            app.DisplayAlerts = false;
            Excel.Workbook wb = app.Workbooks.Add();
            Excel.Worksheet ws = (Excel.Worksheet)wb.Worksheets[1];
            ws.Name = "CSharpCast";
            ((Excel.Range)ws.Range["A1"]).Value2 = "Item";
            ((Excel.Range)ws.Cells[2, 1]).Value2 = 10;
            ((Excel.Range)ws.Cells[3, 1]).Value2 = 32.5;
            ((Excel.Range)ws.Range["A4"]).Formula = "=SUM(A2:A3)";
            Console.WriteLine("  ws.Name = " + ws.Name);
            Console.WriteLine("  A4 = " + ((Excel.Range)ws.Range["A4"]).Value2 + " (expect 42.5)");
            wb.SaveAs(outPath, Excel.XlFileFormat.xlOpenXMLWorkbook);
            wb.Close(false);
        }
        finally
        {
            app.Quit();
        }
    }

    // IN-PROCESS activation (CLSCTX_INPROC_SERVER): COM loads xlcomshim.dll into
    // THIS process and the RCW calls our vtable DIRECTLY -- no proxy, no
    // marshalling, no type library. This is the no-Office (VDI) path, and it also
    // drives our early-bound Range/Worksheet members (out-of-proc the RCW fell
    // back to IDispatch for those). Full create -> write -> formula -> SaveAs.
    private static void RunCastInproc(string outPath)
    {
        const uint CLSCTX_INPROC_SERVER = 0x1;
        Guid clsid = new Guid("7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31"); // shim coclass
        Guid iidUnknown = new Guid("00000000-0000-0000-C000-000000000046");
        IntPtr punk;
        int hr = CoCreateInstance(ref clsid, IntPtr.Zero, CLSCTX_INPROC_SERVER, ref iidUnknown, out punk);
        if (hr < 0) throw new COMException("CoCreateInstance(INPROC_SERVER) failed", hr);
        object obj = Marshal.GetObjectForIUnknown(punk);
        Marshal.Release(punk);
        Console.WriteLine("  loaded shim in-process; casting to Excel._Application...");

        Excel.Application app = (Excel.Application)obj; // vtable QI, in-proc
        try
        {
            Console.WriteLine("  cast OK. Name = " + app.Name + " | Version = " + app.Version);
            app.DisplayAlerts = false;
            Excel.Workbook wb = app.Workbooks.Add();
            Excel.Worksheet ws = (Excel.Worksheet)wb.Worksheets[1];
            ws.Name = "CSharpInproc";
            ((Excel.Range)ws.Range["A1"]).Value2 = "Item";
            ((Excel.Range)ws.Cells[2, 1]).Value2 = 10;
            ((Excel.Range)ws.Cells[3, 1]).Value2 = 32.5;
            ((Excel.Range)ws.Range["A4"]).Formula = "=SUM(A2:A3)";
            Console.WriteLine("  ws.Name = " + ws.Name);
            Console.WriteLine("  A4 = " + ((Excel.Range)ws.Range["A4"]).Value2 + " (expect 42.5)");
            wb.SaveAs(outPath, Excel.XlFileFormat.xlOpenXMLWorkbook);
            wb.Close(false);
        }
        finally
        {
            app.Quit();
        }
    }

    // Just the decisive QueryInterface: activate the shim and cast to the PIA's
    // typed interface, then stop (no method calls). Isolates "does .NET accept our
    // object as Excel._Application" from vtable-slot concerns.
    private static void RunCastOnly()
    {
        const uint CLSCTX_LOCAL_SERVER = 0x4;
        Guid clsid = new Guid("7B3F9E20-4C1A-4E8B-A2D6-9F5C1E0B7A31");
        Guid iidUnknown = new Guid("00000000-0000-0000-C000-000000000046");
        IntPtr punk;
        int hr = CoCreateInstance(ref clsid, IntPtr.Zero, CLSCTX_LOCAL_SERVER, ref iidUnknown, out punk);
        if (hr < 0) throw new COMException("CoCreateInstance(shim CLSID) failed", hr);
        object obj = Marshal.GetObjectForIUnknown(punk);
        Marshal.Release(punk);
        Excel.Application app = (Excel.Application)obj; // QI {000208D5}
        Console.WriteLine("  CAST SUCCEEDED: our object satisfies Excel._Application (QI {000208D5} OK)");
        Marshal.ReleaseComObject(app);
    }

    private static void RunLate(string outPath)
    {
        Type t = Type.GetTypeFromProgID("Excel.Application");
        if (t == null) throw new InvalidOperationException("Excel.Application ProgID is not registered.");
        dynamic app = Activator.CreateInstance(t);
        try
        {
            Console.WriteLine("  Name = " + app.Name + " | Version = " + app.Version);
            app.DisplayAlerts = false;
            dynamic wb = app.Workbooks.Add();
            dynamic ws = wb.Worksheets(1);
            ws.Name = "CSharpLate";
            ws.Range("A1").Value = "Item";
            ws.Cells(2, 1).Value = 10;
            ws.Cells(3, 1).Value = 32.5;
            ws.Range("A4").Formula = "=SUM(A2:A3)";
            Console.WriteLine("  A4 = " + ws.Range("A4").Value2);
            wb.SaveAs(outPath, 51);
            wb.Close(false);
        }
        finally
        {
            app.Quit();
        }
    }
}
