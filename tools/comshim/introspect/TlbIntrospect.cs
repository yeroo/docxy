// Authoritative vtable introspection of Excel's registered type library.
// For each interface we implement, prints every member in vtable (oVft) order
// with its exact parameter list AND per-param flags (FIN/FOUT/FOPT/FLCID/
// FRETVAL). The [lcid] and [retval] hidden params are what a summary dump drops
// but the .NET RCW/proxy pass anyway -- so these are the signatures our Rust
// vtable MUST match byte-for-byte. Output feeds tools/comshim/gen-vtables.ps1.
//
// Usage: TlbIntrospect.exe > excel-vtable.txt
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

internal static class TlbIntrospect
{
    [DllImport("oleaut32.dll", CharSet = CharSet.Unicode)]
    private static extern int LoadRegTypeLib(ref Guid rguid, ushort wVerMajor, ushort wVerMinor, int lcid, out ITypeLib pptlib);

    // Excel typelib LIBID {00020813-0000-0000-C000-000000000046}, v1.x
    private static readonly Guid ExcelLib = new Guid("00020813-0000-0000-C000-000000000046");

    // Interface name -> we print if VT of the ITypeInfo matches one of these.
    private static readonly string[] Wanted =
        { "_Application", "Workbooks", "_Workbook", "Sheets", "_Worksheet", "Range" };

    private static int Main()
    {
        ITypeLib tlb = null;
        for (ushort minor = 9; minor >= 3 && tlb == null; minor--)
        {
            Guid g = ExcelLib;
            try { if (LoadRegTypeLib(ref g, 1, minor, 0, out tlb) == 0 && tlb != null) break; }
            catch { }
        }
        if (tlb == null) { Console.Error.WriteLine("could not load Excel typelib"); return 1; }

        int n = tlb.GetTypeInfoCount();
        for (int i = 0; i < n; i++)
        {
            tlb.GetDocumentation(i, out string name, out _, out _, out _);
            if (Array.IndexOf(Wanted, name) < 0) continue;
            tlb.GetTypeInfo(i, out ITypeInfo ti);
            DumpInterface(name, ti);
        }
        return 0;
    }

    private static string VtName(VarEnum vt) => vt.ToString().Replace("VT_", "");

    private static void DumpInterface(string name, ITypeInfo ti)
    {
        IntPtr pAttr;
        ti.GetTypeAttr(out pAttr);
        var attr = Marshal.PtrToStructure<TYPEATTR>(pAttr);
        Guid iid = attr.guid;
        int cFuncs = attr.cFuncs;
        ti.ReleaseTypeAttr(pAttr);

        Console.WriteLine($"INTERFACE {name} IID={{{iid}}} cFuncs={cFuncs}");
        for (int f = 0; f < cFuncs; f++)
        {
            IntPtr pFd;
            ti.GetFuncDesc(f, out pFd);
            var fd = Marshal.PtrToStructure<FUNCDESC>(pFd);

            // member name (+ param names)
            string[] names = new string[fd.cParams + 1];
            int cn;
            ti.GetNames(fd.memid, names, names.Length, out cn);
            string mname = cn > 0 ? names[0] : "?";

            string kind = fd.invkind switch
            {
                INVOKEKIND.INVOKE_PROPERTYGET => "GET ",
                INVOKEKIND.INVOKE_PROPERTYPUT => "PUT ",
                INVOKEKIND.INVOKE_PROPERTYPUTREF => "PUTREF",
                _ => "FUNC"
            };

            // slot = oVft / pointer-size (8 on x64)
            int slot = fd.oVft / IntPtr.Size;

            var ps = new System.Text.StringBuilder();
            int sz = Marshal.SizeOf<ELEMDESC>();
            for (int p = 0; p < fd.cParams; p++)
            {
                IntPtr pe = fd.lprgelemdescParam + p * sz;
                var ed = Marshal.PtrToStructure<ELEMDESC>(pe);
                VarEnum vt = (VarEnum)ed.tdesc.vt;
                PARAMFLAG fl = (PARAMFLAG)ed.desc.paramdesc.wParamFlags;
                string pn = (p + 1 < cn) ? names[p + 1] : $"a{p}";
                string flags = "";
                if ((fl & PARAMFLAG.PARAMFLAG_FLCID) != 0) flags += "|lcid";
                if ((fl & PARAMFLAG.PARAMFLAG_FRETVAL) != 0) flags += "|retval";
                if ((fl & PARAMFLAG.PARAMFLAG_FOPT) != 0) flags += "|opt";
                if ((fl & PARAMFLAG.PARAMFLAG_FOUT) != 0) flags += "|out";
                if ((fl & PARAMFLAG.PARAMFLAG_FIN) != 0) flags += "|in";
                if (ps.Length > 0) ps.Append(", ");
                ps.Append($"{pn}:{VtName(vt)}{flags}");
            }
            VarEnum rvt = (VarEnum)fd.elemdescFunc.tdesc.vt;
            Console.WriteLine($"  slot#{slot,-3} memid=0x{fd.memid:X8} {kind} {mname}({ps}) -> {VtName(rvt)}");
            ti.ReleaseFuncDesc(pFd);
        }
        Console.WriteLine();
    }
}
