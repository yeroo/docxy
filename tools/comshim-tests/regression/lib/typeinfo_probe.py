"""pywin32 GetTypeInfo probe for the docxy COM shims.

For every object in a typical chain it records GetTypeInfoCount/GetTypeInfo
(name, IID, typekind, #funcs, containing LIBID), whether pywin32's dynamic
Dispatch built its member map from typeinfo, and then tries makepy via
gencache.EnsureDispatch and drives a tiny create+save through it.

  python typeinfo_probe.py word|excel <out.json> <save-path>

gen_py lands under %TEMP% (the runner points TEMP at a per-case dir), so the
user's real gen_py cache is never touched. Exit 0 always; results are in JSON.
"""
import json
import os
import sys
import traceback

kind, out_json, save_path = sys.argv[1], sys.argv[2], sys.argv[3]
res = {"kind": kind, "objects": {}, "dispatch_error": None, "ensure": {}}

try:
    import pythoncom  # noqa: F401
    import win32com
    import win32com.client as wc
    from win32com.client import gencache
except ImportError as e:
    res["unavailable"] = "pywin32 not installed: %s" % e
    json.dump(res, open(out_json, "w"), indent=1)
    print("UNAVAILABLE")
    sys.exit(0)

res["gen_path"] = win32com.__gen_path__


def ti(obj):
    d = {"pyclass": type(obj).__module__ + "." + type(obj).__name__}
    o = getattr(obj, "_oleobj_", obj)
    try:
        d["count"] = o.GetTypeInfoCount()
    except Exception as e:
        d["count_err"] = repr(e)
    try:
        t = o.GetTypeInfo()
        a = t.GetTypeAttr()
        d["name"] = t.GetDocumentation(-1)[0]
        d["iid"] = str(a.iid)
        d["typekind"] = a.typekind
        d["cFuncs"] = a.cFuncs
        d["cVars"] = a.cVars
        try:
            tl, _ = t.GetContainingTypeLib()
            d["libid"] = str(tl.GetLibAttr()[0])
        except Exception as e:
            d["lib_err"] = repr(e)
    except Exception as e:
        d["err"] = repr(e)
    rep = getattr(obj, "_olerepr_", None)
    if rep is not None:
        d["dyn_mapFuncs"] = len(getattr(rep, "mapFuncs", {}) or {})
        d["dyn_propMap"] = len(getattr(rep, "propMap", {}) or {})
    return d


def chain(factory):
    objs = {}
    if kind == "word":
        app = factory("Word.Application")
        objs["Application"] = app
        docs = app.Documents
        objs["Documents"] = docs
        doc = docs.Add()
        objs["Document"] = doc
        sel = app.Selection
        objs["Selection"] = sel
        objs["Font"] = sel.Font
        objs["ParagraphFormat"] = sel.ParagraphFormat
        objs["Range"] = doc.Content
        objs["Tables"] = doc.Tables
        return app, doc, objs
    app = factory("Excel.Application")
    objs["Application"] = app
    wbs = app.Workbooks
    objs["Workbooks"] = wbs
    wb = wbs.Add()
    objs["Workbook"] = wb
    objs["Worksheets"] = wb.Worksheets
    ws = wb.Worksheets(1)
    objs["Worksheet"] = ws
    rng = ws.Range("A1")
    objs["Range"] = rng
    objs["Font"] = rng.Font
    objs["Interior"] = rng.Interior
    return app, wb, objs


def finish(app, doc, write):
    if kind == "word":
        if write:
            app.Selection.TypeText("TypeInfoProbe")
            doc.SaveAs2(save_path)
        doc.Close()
    else:
        if write:
            doc.Worksheets(1).Range("A1").Value = "TypeInfoProbe"
            doc.SaveAs(save_path, 51)
        doc.Close(False)
    app.Quit()


# 1) dynamic Dispatch + per-object GetTypeInfo
try:
    app, doc, objs = chain(wc.Dispatch)
    for k, v in objs.items():
        res["objects"][k] = ti(v)
    finish(app, doc, False)
    del app, doc, objs
except Exception as e:
    res["dispatch_error"] = "%s: %s" % (type(e).__name__, e)
    res["dispatch_tb"] = traceback.format_exc()

PROGID = "Word.Application" if kind == "word" else "Excel.Application"


def is_gen(o):
    return type(o).__module__.startswith("win32com.gen_py")


def makepy_phase(key, factory, write):
    r = res.setdefault(key, {})
    try:
        app, doc, objs = chain(factory)
        r["classes"] = {k: type(v).__module__ + "." + type(v).__name__ for k, v in objs.items()}
        r["gen_classes"] = sorted(k for k, v in objs.items() if is_gen(v))
        finish(app, doc, write)
        r["ok"] = True
        if write:
            r["saved"] = os.path.exists(save_path)
    except Exception as e:
        r["ok"] = False
        r["error"] = "%s: %s" % (type(e).__name__, e)
        r["tb"] = traceback.format_exc()
    r["gen_modules"] = sorted(
        n for n in os.listdir(win32com.__gen_path__) if not n.startswith("__")
    ) if os.path.isdir(win32com.__gen_path__) else []


# EnsureDispatch decides "makepy support already present" via hasattr(disp,
# "CLSID") on the dynamic CDispatch; record what the shim answers for that name.
try:
    d = wc.Dispatch(PROGID)
    try:
        v = d.CLSID
        res["cdispatch_CLSID_attr"] = "resolved -> %s" % type(v).__name__
    except AttributeError:
        res["cdispatch_CLSID_attr"] = "AttributeError"
    d.Quit()
    del d
except Exception as e:
    res["cdispatch_CLSID_attr"] = "error %s: %s" % (type(e).__name__, e)

# 2) gencache.EnsureDispatch exactly as a client calls it (fresh gen_py)
makepy_phase("ensure", gencache.EnsureDispatch, False)

# 3) explicit makepy: EnsureModule(LIBID from the object's typeinfo), then
#    Dispatch should return generated classes; drive a create + save through them.
try:
    d = wc.Dispatch(PROGID)
    tl, _ = d._oleobj_.GetTypeInfo().GetContainingTypeLib()
    la = tl.GetLibAttr()
    res["explicit_module"] = str(gencache.EnsureModule(la[0], la[1], la[3], la[4], bForDemand=1))
    d.Quit()
    del d
except Exception as e:
    res["explicit_module_error"] = "%s: %s" % (type(e).__name__, e)
makepy_phase("explicit", wc.Dispatch, True)

objs_ok = [k for k, v in res["objects"].items() if "err" not in v and v.get("count") == 1]
res["summary"] = {
    "objects_total": len(res["objects"]),
    "objects_with_typeinfo": len(objs_ok),
    "missing_typeinfo": [k for k in res["objects"] if k not in objs_ok],
    "cdispatch_CLSID_attr": res.get("cdispatch_CLSID_attr"),
    "ensure_ok": res["ensure"].get("ok", False),
    "ensure_used_makepy": bool(res["ensure"].get("gen_classes")),
    "explicit_ok": res.get("explicit", {}).get("ok", False),
    "explicit_gen_classes": res.get("explicit", {}).get("gen_classes", []),
    "explicit_saved": res.get("explicit", {}).get("saved", False),
}
json.dump(res, open(out_json, "w"), indent=1)
print(json.dumps(res["summary"]))
print("TYPEINFO PROBE: done")
