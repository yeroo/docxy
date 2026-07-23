// Early-bound Word automation from C# via the Word PIA (the demanding binding a
// .NET host uses): activate the shim by its own CLSID, cast to Word._Application
// (QI its dual IID), and drive the create path. If a cast/call needs a vtable
// interface the shim only exposes as IDispatch, it throws — telling us exactly
// which objects must become dual. A clean pass prints RESULT: OK.
//
//   WordInteropTest.exe castshim <out.docx>
using System;
using System.IO;
using System.Runtime.InteropServices;
using Word = Microsoft.Office.Interop.Word;

internal static class WordInteropTest
{
    [DllImport("ole32.dll")]
    private static extern int CoCreateInstance(
        ref Guid clsid, IntPtr outer, uint clsctx, ref Guid iid, out IntPtr obj);

    private static int Main(string[] args)
    {
        string mode = args.Length > 0 ? args[0].ToLowerInvariant() : "castshim";
        string outPath = args.Length > 1 ? args[1] : Path.Combine(Path.GetTempPath(), "cs-word.docx");
        try
        {
            RunCastShim(outPath);
            Console.WriteLine("RESULT: OK (" + mode + ") -> " + outPath);
            return 0;
        }
        catch (Exception ex)
        {
            Console.WriteLine("RESULT: FAIL (" + mode + "): " + ex.GetType().FullName + ": " + ex.Message);
            return 1;
        }
    }

    private static void RunCastShim(string outPath)
    {
        const uint CLSCTX_LOCAL_SERVER = 0x4;
        Guid clsid = new Guid("9C2F4A10-7D33-4B6E-B1A4-2E7C8D5F0A92"); // our shim coclass
        Guid iidUnknown = new Guid("00000000-0000-0000-C000-000000000046");
        IntPtr punk;
        int hr = CoCreateInstance(ref clsid, IntPtr.Zero, CLSCTX_LOCAL_SERVER, ref iidUnknown, out punk);
        if (hr < 0) throw new COMException("CoCreateInstance(shim CLSID) failed", hr);
        object obj = Marshal.GetObjectForIUnknown(punk);
        Marshal.Release(punk);
        Console.WriteLine("  activated shim; casting to Word._Application (QI {00020970})...");

        Word._Application app = (Word._Application)obj; // THE cast under test
        try
        {
            Console.WriteLine("  cast OK. Name = " + app.Name + " | Version = " + app.Version);
            Word.Documents docs = app.Documents;
            Word.Document doc = docs.Add();
            Word.Selection sel = app.Selection;
            sel.TypeText("Early-bound Word shim");
            sel.TypeParagraph();
            sel.TypeText("Second paragraph.");
            object fn = outPath;
            object missing = Type.Missing;
            doc.SaveAs2(ref fn, ref missing, ref missing, ref missing, ref missing, ref missing,
                        ref missing, ref missing, ref missing, ref missing, ref missing, ref missing,
                        ref missing, ref missing, ref missing, ref missing, ref missing);
            object save = false;
            doc.Close(ref save, ref missing, ref missing);
        }
        finally
        {
            object missing = Type.Missing;
            app.Quit(ref missing, ref missing, ref missing);
        }
    }
}
