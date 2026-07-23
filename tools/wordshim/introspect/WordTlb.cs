// Vtable introspection of Word's registered type library — the Word counterpart
// of tools/comshim/introspect/TlbIntrospect.cs. Two modes:
//   list           -> every dual interface (name, kind, IID, cFuncs): discovery
//   <no arg>        -> dump the WANTED interfaces with per-param flags (FIN/FOUT/
//                      FOPT/FLCID/FRETVAL) in oVft order -> word-vtable.txt
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

internal static class WordTlb
{
    [DllImport("oleaut32.dll", CharSet = CharSet.Unicode)]
    private static extern int LoadRegTypeLib(ref Guid rguid, ushort wVerMajor, ushort wVerMinor, int lcid, out ITypeLib pptlib);

    // Microsoft Word Object Library, LIBID {00020905-...}, v8.x.
    private static readonly Guid WordLib = new Guid("00020905-0000-0000-C000-000000000046");

    private static readonly string[] Wanted =
        { "_Application", "Documents", "_Document", "Selection", "Range", "_Font", "_ParagraphFormat" };

    private static int Main(string[] args)
    {
        bool list = args.Length > 0 && args[0] == "list";
        ITypeLib tlb = null;
        for (ushort minor = 9; minor >= 0 && tlb == null; minor--)
        {
            Guid g = WordLib;
            try { if (LoadRegTypeLib(ref g, 8, minor, 0, out tlb) == 0 && tlb != null) break; }
            catch { }
        }
        if (tlb == null) { Console.Error.WriteLine("could not load Word typelib"); return 1; }

        int n = tlb.GetTypeInfoCount();
        for (int i = 0; i < n; i++)
        {
            tlb.GetDocumentation(i, out string name, out _, out _, out _);
            tlb.GetTypeInfo(i, out ITypeInfo ti);
            IntPtr pAttr; ti.GetTypeAttr(out pAttr);
            var attr = Marshal.PtrToStructure<TYPEATTR>(pAttr);
            var (kind, iid, cf) = (attr.typekind, attr.guid, attr.cFuncs);
            ti.ReleaseTypeAttr(pAttr);

            if (list)
            {
                bool iface = kind == TYPEKIND.TKIND_DISPATCH || kind == TYPEKIND.TKIND_INTERFACE;
                if (iface && cf > 8)
                    Console.WriteLine($"#{i,-4} kind={(int)kind} cFuncs={cf,-4} {{{iid}}} {name}");
                continue;
            }
            if (Array.IndexOf(Wanted, name) < 0) continue;
            DumpInterface(name, ti, kind, iid, cf);
        }
        return 0;
    }

    private static string VtName(VarEnum vt) => vt.ToString().Replace("VT_", "");

    private static void DumpInterface(string name, ITypeInfo ti, TYPEKIND kind, Guid iid, int cFuncs)
    {
        Console.WriteLine($"INTERFACE {name} IID={{{iid}}} kind={(int)kind} cFuncs={cFuncs}");
        int sz = Marshal.SizeOf<ELEMDESC>();
        for (int f = 0; f < cFuncs; f++)
        {
            IntPtr pFd; ti.GetFuncDesc(f, out pFd);
            var fd = Marshal.PtrToStructure<FUNCDESC>(pFd);
            string[] names = new string[fd.cParams + 1];
            int cn; ti.GetNames(fd.memid, names, names.Length, out cn);
            string mname = cn > 0 ? names[0] : "?";
            string k = fd.invkind switch
            {
                INVOKEKIND.INVOKE_PROPERTYGET => "GET ",
                INVOKEKIND.INVOKE_PROPERTYPUT => "PUT ",
                INVOKEKIND.INVOKE_PROPERTYPUTREF => "PUTREF",
                _ => "FUNC"
            };
            int slot = fd.oVft / IntPtr.Size;
            var ps = new System.Text.StringBuilder();
            for (int p = 0; p < fd.cParams; p++)
            {
                var ed = Marshal.PtrToStructure<ELEMDESC>(fd.lprgelemdescParam + p * sz);
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
            Console.WriteLine($"  slot#{slot,-3} memid=0x{fd.memid:X8} {k} {mname}({ps}) -> {VtName(rvt)}");
            ti.ReleaseFuncDesc(pFd);
        }
        Console.WriteLine();
    }
}
