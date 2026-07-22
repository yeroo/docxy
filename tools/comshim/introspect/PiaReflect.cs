// Authoritative CLIENT-side ABI: reflect the Excel PIA (the same assembly the C#
// test -- and Petrel-style .NET callers -- bind against). For each interface we
// implement, list every member in vtable order (metadata order == vtable order
// for a ComImport dual interface; base = 7 = IUnknown(3)+IDispatch(4)) with:
//   - the computed vtable slot,
//   - its [DispId],
//   - whether it carries [LCIDConversion(pos)] -- meaning the CLR INJECTS an
//     lcid (I4) argument at `pos`, which the live typelib does NOT show but the
//     RCW pushes anyway (this is what made get_Version fault: lcid landed in our
//     retval pointer),
//   - the managed parameter types.
// This is the ground truth our Rust vtable signatures must match. C# 5 / .NET
// Framework so the old csc.exe can compile it and load the Framework PIA.
//
// Usage: PiaReflect.exe > excel-pia.txt
using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;

internal static class PiaReflect
{
    private static readonly string[] Wanted =
        { "_Application", "Workbooks", "_Workbook", "Sheets", "_Worksheet", "Range" };

    private static int Main()
    {
        Assembly pia = Assembly.Load(
            "Microsoft.Office.Interop.Excel, Version=15.0.0.0, Culture=neutral, PublicKeyToken=71e9bce111e9429c");
        foreach (string want in Wanted)
        {
            Type t = pia.GetType("Microsoft.Office.Interop.Excel." + want);
            if (t == null) { Console.WriteLine("MISSING " + want); continue; }
            DumpInterface(want, t);
        }
        return 0;
    }

    private static void DumpInterface(string name, Type t)
    {
        object[] g = t.GetCustomAttributes(typeof(GuidAttribute), false);
        string iid = g.Length > 0 ? ((GuidAttribute)g[0]).Value : "?";
        Console.WriteLine("INTERFACE " + name + " IID={" + iid + "}");

        MethodInfo[] methods = t.GetMethods(BindingFlags.Public | BindingFlags.Instance | BindingFlags.DeclaredOnly);
        Array.Sort(methods, delegate(MethodInfo a, MethodInfo b) { return a.MetadataToken.CompareTo(b.MetadataToken); });

        int idx = 0;
        foreach (MethodInfo m in methods)
        {
            int slot = 7 + idx;
            idx++;

            int dispid = -999;
            object[] d = m.GetCustomAttributes(typeof(DispIdAttribute), false);
            if (d.Length > 0) dispid = ((DispIdAttribute)d[0]).Value;

            int lcidPos = -1;
            object[] lc = m.GetCustomAttributes(typeof(LCIDConversionAttribute), false);
            if (lc.Length > 0) lcidPos = ((LCIDConversionAttribute)lc[0]).Value;

            StringBuilder ps = new StringBuilder();
            foreach (ParameterInfo p in m.GetParameters())
            {
                if (ps.Length > 0) ps.Append(", ");
                string ty = p.ParameterType.Name;
                string fl = "";
                if (p.IsOptional) fl += "|opt";
                if (p.IsOut) fl += "|out";
                if (p.ParameterType.IsByRef) fl += "|ref";
                ps.Append(p.Name + ":" + ty + fl);
            }
            string ret = m.ReturnType == typeof(void) ? "void" : m.ReturnType.Name;
            string lcidStr = lcidPos >= 0 ? (" LCID@" + lcidPos) : "";
            Console.WriteLine(
                "  slot#" + slot.ToString().PadRight(3) +
                " dispid=" + dispid +
                lcidStr +
                "  " + m.Name + "(" + ps + ") -> " + ret);
        }
        Console.WriteLine();
    }
}
