// Word PIA reflection (counterpart of tools/comshim/introspect/PiaReflect.cs).
// For each Word interface: its IID, its InterfaceType (Dual => a vtable interface
// the client casts to and calls; IsIDispatch => dispinterface, IDispatch-only),
// and per member the vtable slot, [DispId], and [LCIDConversion] position (the
// hidden lcid the CLR injects). This decides which objects the shim must expose as
// dual vtables vs plain IDispatch, and where lcid params go.
// C# 5 / .NET Framework so the old csc.exe compiles it and loads the Framework PIA.
using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;

internal static class WordPia
{
    private static readonly string[] Wanted =
        { "_Application", "Documents", "_Document", "Selection", "Range", "Paragraphs" };

    private static int Main()
    {
        Assembly pia = Assembly.Load(
            "Microsoft.Office.Interop.Word, Version=15.0.0.0, Culture=neutral, PublicKeyToken=71e9bce111e9429c");
        foreach (string want in Wanted)
        {
            Type t = pia.GetType("Microsoft.Office.Interop.Word." + want);
            if (t == null) { Console.WriteLine("MISSING " + want); continue; }
            DumpInterface(want, t);
        }
        return 0;
    }

    private static void DumpInterface(string name, Type t)
    {
        object[] g = t.GetCustomAttributes(typeof(GuidAttribute), false);
        string iid = g.Length > 0 ? ((GuidAttribute)g[0]).Value : "?";

        string itype = "?";
        object[] it = t.GetCustomAttributes(typeof(InterfaceTypeAttribute), false);
        if (it.Length > 0)
        {
            ComInterfaceType c = ((InterfaceTypeAttribute)it[0]).Value;
            itype = c.ToString();   // InterfaceIsDual / InterfaceIsIDispatch / InterfaceIsIUnknown
        }
        Console.WriteLine("INTERFACE " + name + " IID={" + iid + "} " + itype);

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
                string fl = "";
                if (p.IsOptional) fl += "|opt";
                if (p.IsOut) fl += "|out";
                if (p.ParameterType.IsByRef) fl += "|ref";
                ps.Append(p.Name + ":" + p.ParameterType.Name + fl);
            }
            string ret = m.ReturnType == typeof(void) ? "void" : m.ReturnType.Name;
            string lcidStr = lcidPos >= 0 ? (" LCID@" + lcidPos) : "";
            Console.WriteLine(
                "  slot#" + slot.ToString().PadRight(3) + " dispid=" + dispid + lcidStr +
                "  " + m.Name + "(" + ps + ") -> " + ret);
        }
        Console.WriteLine();
    }
}
