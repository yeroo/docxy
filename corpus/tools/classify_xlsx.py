#!/usr/bin/env python3
"""Classify every .xlsx in the corpus by the SpreadsheetML features — and the
worksheet-function families — it exercises, so the compare launcher can browse
them by category, feature tag, or folder.

By default this classifies the workbooks already sitting in ``corpus/xlsx/``
in place. Point it at an external tree of tricky workbooks with ``--src DIR``
and it copies them into ``corpus/xlsx-ext/`` first, preserving structure.

Output:
  corpus/classification-xlsx.json   (manifest, same schema as classify.py)

Run:  python corpus/tools/classify_xlsx.py [--src DIR]
"""

import argparse
import json
import os
import re
import shutil
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.dirname(HERE)  # corpus/
MANIFEST = os.path.join(CORPUS, "classification-xlsx.json")

# Human-friendly descriptions for each feature tag (shown in the launcher).
TAG_DOC = {
    # structural / package features
    "formulas": "Cells with formulas (<f>)",
    "shared-formulas": "Shared-formula groups (<f t=\"shared\">)",
    "array-formulas": "Legacy CSE array formulas (<f t=\"array\">)",
    "dynamic-arrays": "Dynamic arrays / spill (_xlfn cell metadata)",
    "tables": "Structured tables (xl/tables)",
    "pivot-tables": "Pivot tables",
    "pivot-cache": "Pivot caches",
    "data-model": "Power-Pivot / gridcore data model",
    "charts": "Charts",
    "drawings": "DrawingML / shapes",
    "images": "Embedded raster images",
    "defined-names": "Defined names / named ranges",
    "external-links": "External workbook links",
    "conditional-formatting": "Conditional formatting rules",
    "data-validation": "Data validation",
    "merged-cells": "Merged cell ranges",
    "hyperlinks": "Hyperlinks",
    "auto-filter": "AutoFilter",
    "frozen-panes": "Frozen/split panes",
    "comments": "Cell comments / threaded comments",
    "number-formats": "Custom number formats",
    "multi-sheet": "More than one worksheet",
    "cross-sheet-refs": "References across sheets (Sheet!A1)",
    "3d-refs": "3-D references (Sheet1:Sheet3!A1)",
    "date-system-1904": "1904 date system",
    "volatile": "Volatile functions (NOW/TODAY/RAND/OFFSET…)",
    "lambda": "LAMBDA and helper functions",
    "protected": "Sheet/workbook protection",
    "empty": "Essentially empty workbook",
    # function-family tags (which worksheet-function areas the file exercises)
    "fn-math": "Math & trigonometry functions",
    "fn-stat": "Statistical functions",
    "fn-financial": "Financial functions",
    "fn-text": "Text functions",
    "fn-datetime": "Date & time functions",
    "fn-lookup": "Lookup & reference functions",
    "fn-logical": "Logical functions",
    "fn-info": "Information functions",
    "fn-engineering": "Engineering functions",
    "fn-database": "Database functions",
    "fn-dynamic-array": "Dynamic-array functions (FILTER/SORT/…)",
}

# Worksheet functions grouped by family. Used to tag which areas a file covers.
FN_FAMILIES = {
    "fn-math": {
        "ABS", "ACOS", "ACOSH", "ACOT", "ARABIC", "ASIN", "ATAN", "ATAN2",
        "BASE", "CEILING", "CEILING.MATH", "COMBIN", "COMBINA", "COS", "COSH",
        "COT", "CSC", "DECIMAL", "DEGREES", "EVEN", "EXP", "FACT", "FACTDOUBLE",
        "FLOOR", "FLOOR.MATH", "GCD", "INT", "LCM", "LN", "LOG", "LOG10",
        "MDETERM", "MINVERSE", "MMULT", "MOD", "MROUND", "MULTINOMIAL", "ODD",
        "PI", "POWER", "PRODUCT", "QUOTIENT", "RADIANS", "RAND", "RANDARRAY",
        "RANDBETWEEN", "ROMAN", "ROUND", "ROUNDDOWN", "ROUNDUP", "SEC", "SERIESSUM",
        "SIGN", "SIN", "SINH", "SQRT", "SQRTPI", "SUM", "SUMIF", "SUMIFS",
        "SUMPRODUCT", "SUMSQ", "SUMX2MY2", "SUMX2PY2", "SUMXMY2", "TAN", "TANH",
        "TRUNC",
    },
    "fn-stat": {
        "AVEDEV", "AVERAGE", "AVERAGEA", "AVERAGEIF", "AVERAGEIFS", "CORREL",
        "COUNT", "COUNTA", "COUNTBLANK", "COUNTIF", "COUNTIFS", "COVAR",
        "COVARIANCE.P", "COVARIANCE.S", "DEVSQ", "FISHER", "FISHERINV",
        "FORECAST", "FORECAST.LINEAR", "GEOMEAN", "HARMEAN", "INTERCEPT", "KURT",
        "LARGE", "MAX", "MAXA", "MAXIFS", "MEDIAN", "MIN", "MINA", "MINIFS",
        "MODE", "MODE.SNGL", "PEARSON", "PERCENTILE", "PERCENTILE.INC",
        "PERCENTILE.EXC", "PERCENTRANK", "PERCENTRANK.INC", "PERMUT", "QUARTILE",
        "QUARTILE.INC", "QUARTILE.EXC", "RANK", "RANK.EQ", "RSQ", "SKEW", "SLOPE",
        "SMALL", "STANDARDIZE", "STDEV", "STDEV.P", "STDEV.S", "STDEVA", "STDEVP",
        "STDEVPA", "TRIMMEAN", "VAR", "VAR.P", "VAR.S", "VARA", "VARP", "VARPA",
    },
    "fn-financial": {
        "CUMIPMT", "CUMPRINC", "DB", "DDB", "DOLLARDE", "DOLLARFR", "EFFECT",
        "FV", "IPMT", "IRR", "ISPMT", "MIRR", "NOMINAL", "NPER", "NPV",
        "PDURATION", "PMT", "PPMT", "PV", "RATE", "RRI", "SLN", "SYD", "XIRR",
        "XNPV",
    },
    "fn-text": {
        "CHAR", "CLEAN", "CODE", "CONCAT", "CONCATENATE", "DOLLAR", "EXACT",
        "FIND", "FIXED", "LEFT", "LEN", "LOWER", "MID", "NUMBERVALUE", "PROPER",
        "REPLACE", "REPT", "RIGHT", "SEARCH", "SUBSTITUTE", "T", "TEXT",
        "TEXTAFTER", "TEXTBEFORE", "TEXTJOIN", "TEXTSPLIT", "TRIM", "UNICHAR",
        "UNICODE", "UPPER", "VALUE",
    },
    "fn-datetime": {
        "DATE", "DATEDIF", "DATEVALUE", "DAY", "DAYS", "DAYS360", "EDATE",
        "EOMONTH", "HOUR", "ISOWEEKNUM", "MINUTE", "MONTH", "NETWORKDAYS",
        "NETWORKDAYS.INTL", "NOW", "SECOND", "TIME", "TIMEVALUE", "TODAY",
        "WEEKDAY", "WEEKNUM", "WORKDAY", "WORKDAY.INTL", "YEAR", "YEARFRAC",
    },
    "fn-lookup": {
        "ADDRESS", "CHOOSE", "CHOOSECOLS", "CHOOSEROWS", "COLUMN", "COLUMNS",
        "HLOOKUP", "HYPERLINK", "INDEX", "INDIRECT", "LOOKUP", "MATCH", "OFFSET",
        "ROW", "ROWS", "TRANSPOSE", "VLOOKUP", "XLOOKUP", "XMATCH",
    },
    "fn-logical": {
        "AND", "FALSE", "IF", "IFERROR", "IFNA", "IFS", "NOT", "OR", "SWITCH",
        "TRUE", "XOR",
    },
    "fn-info": {
        "CELL", "ERROR.TYPE", "ISBLANK", "ISERR", "ISERROR", "ISEVEN",
        "ISFORMULA", "ISLOGICAL", "ISNA", "ISNONTEXT", "ISNUMBER", "ISODD",
        "ISREF", "ISTEXT", "N", "NA", "SHEET", "SHEETS", "TYPE",
    },
    "fn-engineering": {
        "BIN2DEC", "BIN2HEX", "BIN2OCT", "BITAND", "BITLSHIFT", "BITOR",
        "BITRSHIFT", "BITXOR", "DEC2BIN", "DEC2HEX", "DEC2OCT", "DELTA", "GESTEP",
        "HEX2BIN", "HEX2DEC", "HEX2OCT", "OCT2BIN", "OCT2DEC", "OCT2HEX",
    },
    "fn-database": {
        "DAVERAGE", "DCOUNT", "DCOUNTA", "DGET", "DMAX", "DMIN", "DPRODUCT",
        "DSTDEV", "DSTDEVP", "DSUM", "DVAR", "DVARP",
    },
    "fn-dynamic-array": {
        "BYCOL", "BYROW", "DROP", "EXPAND", "FILTER", "HSTACK", "MAKEARRAY",
        "MAP", "REDUCE", "SCAN", "SEQUENCE", "SORT", "SORTBY", "TAKE", "TOCOL",
        "TOROW", "UNIQUE", "VSTACK", "WRAPCOLS", "WRAPROWS",
    },
}

VOLATILE = {"NOW", "TODAY", "RAND", "RANDBETWEEN", "RANDARRAY", "OFFSET", "INDIRECT", "CELL", "INFO"}

# Function name inside a formula: an uppercase token right before '('.
FN_RE = re.compile(r"(?:_xlfn\.|_xlws\.)?([A-Z][A-Z0-9_.]*)\s*\(")


def read(z, name):
    try:
        return z.read(name).decode("utf-8", "replace")
    except Exception:
        return ""


def extract_functions(sheet_xml):
    """Every function name called inside a worksheet's <f> elements."""
    fns = set()
    for m in re.finditer(r"<f\b[^>]*>(.*?)</f>", sheet_xml, re.S):
        for fm in FN_RE.finditer(m.group(1)):
            fns.add(fm.group(1).upper())
    return fns


def classify(path):
    """Return a sorted list of feature tags for the .xlsx at `path`."""
    tags = set()
    try:
        z = zipfile.ZipFile(path)
    except Exception:
        return ["encrypted"]
    names = z.namelist()
    if not any(n.endswith("workbook.xml") for n in names):
        return ["encrypted"]

    def has(pred):
        return any(pred(n) for n in names)

    # ---- part-name based ----
    if has(lambda n: "/tables/" in n and n.endswith(".xml")):
        tags.add("tables")
    if has(lambda n: "/pivotTables/" in n):
        tags.add("pivot-tables")
    if has(lambda n: "/pivotCache/" in n):
        tags.add("pivot-cache")
    if has(lambda n: n.endswith("gridcoreModel.xml") or "/model/" in n or "DataModel" in n):
        tags.add("data-model")
    if has(lambda n: "/charts/" in n or re.search(r"chart\d*\.xml$", n)):
        tags.add("charts")
    if has(lambda n: "/drawings/" in n and n.endswith(".xml")):
        tags.add("drawings")
    if has(lambda n: re.search(r"/media/.*\.(png|jpe?g|gif|bmp|tif|emf|wmf)$", n, re.I)):
        tags.add("images")
    if has(lambda n: "externalLinks" in n):
        tags.add("external-links")
    if has(lambda n: n.endswith("/comments.xml") or "threadedComment" in n):
        tags.add("comments")

    # ---- workbook.xml ----
    wb = read(z, "xl/workbook.xml")
    sheet_count = wb.count("<sheet ")
    if sheet_count > 1:
        tags.add("multi-sheet")
    if "<definedName" in wb:
        tags.add("defined-names")
    if 'date1904="1"' in wb or 'date1904="true"' in wb.lower():
        tags.add("date-system-1904")
    # Only real protection — LibreOffice emits an empty <workbookProtection/>.
    if re.search(
        r"workbookProtection[^>/]*\b(lockStructure|lockWindows|workbookPassword|"
        r"workbookAlgorithmName)=\"(1|true|[^\"]*[A-Za-z0-9])\"",
        wb,
    ):
        tags.add("protected")

    # ---- styles: custom number formats ----
    styles = read(z, "xl/styles.xml")
    if "<numFmt " in styles:
        tags.add("number-formats")

    # ---- per-sheet scan ----
    all_fns = set()
    total_f = 0
    for n in names:
        if not re.search(r"xl/worksheets/sheet\d*\.xml$", n):
            continue
        s = read(z, n)
        total_f += s.count("<f")
        if "<f" in s:
            tags.add("formulas")
        if 't="shared"' in s:
            tags.add("shared-formulas")
        if 't="array"' in s:
            tags.add("array-formulas")
        if "conditionalFormatting" in s:
            tags.add("conditional-formatting")
        if "<dataValidation" in s:
            tags.add("data-validation")
        if "<mergeCell " in s:
            tags.add("merged-cells")
        if "<hyperlink " in s:
            tags.add("hyperlinks")
        if "<autoFilter" in s:
            tags.add("auto-filter")
        if 'state="frozen"' in s or "<pane " in s:
            tags.add("frozen-panes")
        if "sheetProtection" in s:
            tags.add("protected")
        # cross-sheet and 3-D references live in the formula text
        if re.search(r"[A-Za-z0-9_']+!\$?[A-Z]", s):
            tags.add("cross-sheet-refs")
        if re.search(r"[A-Za-z0-9_']+:[A-Za-z0-9_']+!", s):
            tags.add("3d-refs")
        all_fns |= extract_functions(s)

    # cell metadata columns (cm=/vm=) mark dynamic-array anchors
    if has(lambda n: n.endswith("metadata.xml")) or has(lambda n: "richData" in n):
        tags.add("dynamic-arrays")

    # ---- function-family tags ----
    for fam, members in FN_FAMILIES.items():
        if all_fns & members:
            tags.add(fam)
    if all_fns & FN_FAMILIES["fn-dynamic-array"]:
        tags.add("dynamic-arrays")
    if all_fns & {"LAMBDA", "MAP", "REDUCE", "SCAN", "BYROW", "BYCOL", "MAKEARRAY", "ISOMITTED"}:
        tags.add("lambda")
    if all_fns & VOLATILE:
        tags.add("volatile")

    if not tags or (total_f == 0 and "tables" not in tags and "pivot-tables" not in tags):
        tags.add("empty")

    return sorted(tags), sorted(all_fns)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=None, help="external tree of .xlsx to import")
    args = ap.parse_args()

    if args.src:
        src = os.path.abspath(args.src)
        if not os.path.isdir(src):
            raise SystemExit(f"source not found: {src}")
        root = "xlsx-ext"
        base = os.path.join(CORPUS, root)
        if os.path.isdir(base):
            shutil.rmtree(base)
        os.makedirs(base, exist_ok=True)
        copy = True
    else:
        src = os.path.join(CORPUS, "xlsx")
        root = "xlsx"
        base = src
        copy = False
        if not os.path.isdir(src):
            raise SystemExit(f"no corpus/xlsx directory; pass --src DIR")

    entries = []
    tag_counts = {}
    cat_counts = {}
    for r, _dirs, fnames in os.walk(src):
        for fn in fnames:
            if not fn.lower().endswith(".xlsx") or fn.startswith("~$"):
                continue
            spath = os.path.join(r, fn)
            rel = os.path.relpath(spath, src).replace("\\", "/")
            rel_dir = os.path.dirname(rel)
            # Category: first folder, else the filename stem prefix (calc-*, feat-*).
            if "/" in rel:
                category = rel.split("/")[0]
            else:
                category = fn.split("-", 1)[0] if "-" in fn else "misc"

            if copy:
                dpath = os.path.join(base, rel.replace("/", os.sep))
                os.makedirs(os.path.dirname(dpath), exist_ok=True)
                shutil.copy2(spath, dpath)

            result = classify(spath)
            tags = result[0] if isinstance(result, tuple) else result
            fns = result[1] if isinstance(result, tuple) else []
            entries.append(
                {
                    "name": fn,
                    "path": root + "/" + rel,
                    "folder": rel_dir,
                    "category": category,
                    "tags": tags,
                    "functions": fns,
                    "size": os.path.getsize(spath),
                }
            )
            cat_counts[category] = cat_counts.get(category, 0) + 1
            for t in tags:
                tag_counts[t] = tag_counts.get(t, 0) + 1

    entries.sort(key=lambda e: (e["category"], e["folder"], e["name"]))
    manifest = {
        "root": root,
        "count": len(entries),
        "categories": dict(sorted(cat_counts.items())),
        "tags": {
            t: {"count": tag_counts[t], "doc": TAG_DOC.get(t, "")}
            for t in sorted(tag_counts, key=lambda k: (-tag_counts[k], k))
        },
        "files": entries,
    }
    with open(MANIFEST, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)

    print(f"classified {len(entries)} workbooks into {len(cat_counts)} categories, {len(tag_counts)} tags")
    if manifest["tags"]:
        print("top tags:", ", ".join(f"{t}({tag_counts[t]})" for t in list(manifest["tags"])[:12]))


if __name__ == "__main__":
    main()
