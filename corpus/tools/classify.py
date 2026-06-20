#!/usr/bin/env python3
"""Copy the OOXML tricky-file corpus into the repo and classify every .docx by
the WordprocessingML features (and known bug/edge cases) it contains.

Output:
  corpus/files/<original folder structure>/<file>.docx   (copies)
  corpus/classification.json                              (manifest)

Run:  python corpus/tools/classify.py
"""

import json
import os
import re
import shutil
import zipfile

SRC = r"C:\Users\boris\source\Fast365\tests\corpus\openxml-sdk\test\DocumentFormat.OpenXml.Tests.Assets\assets\TestDataStorage\v2FxTestFiles\wordprocessing"
HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.dirname(HERE)            # corpus/
FILES = os.path.join(CORPUS, "files")
MANIFEST = os.path.join(CORPUS, "classification.json")

# Human-friendly descriptions for each tag (shown in the launcher).
TAG_DOC = {
    "comments": "Review comments (commentReference / comments.xml)",
    "tracked-changes": "Tracked insertions/deletions (w:ins / w:del)",
    "footnotes": "Footnotes",
    "endnotes": "Endnotes",
    "tables": "Tables",
    "merged-cells": "Tables with merged cells (gridSpan / vMerge)",
    "hyperlinks": "Hyperlinks",
    "bookmarks": "Bookmarks",
    "fields": "Field codes (fldChar / fldSimple)",
    "toc": "Table of contents field",
    "lists": "Numbered/bulleted lists (numPr)",
    "images": "Embedded raster images",
    "wmf-emf": "WMF/EMF vector images",
    "vml": "Legacy VML shapes (v:shape)",
    "textbox": "Text boxes (txbxContent)",
    "watermark": "Page watermark / background",
    "ole": "Embedded OLE objects",
    "chart": "Charts",
    "smartart": "SmartArt diagrams",
    "math": "Equations / OMML math (m:oMath)",
    "sdt": "Content controls / structured document tags",
    "smarttag": "Smart tags (w:smartTag)",
    "symbols": "Symbol runs (w:sym)",
    "multi-column": "Multi-column section layout",
    "page-borders": "Page borders",
    "landscape": "Landscape orientation",
    "rtl": "Right-to-left / bidi text",
    "shading": "Paragraph/cell shading",
    "drawing": "DrawingML graphics (w:drawing)",
    "headers-footers": "Headers and/or footers",
    "title-page": "Different first-page header/footer",
    "even-odd": "Different odd/even headers/footers",
    "section-breaks": "Multiple section properties",
    "numbering-part": "numbering.xml present",
    "custom-xml": "Custom XML data parts",
    "protected": "Editing-protected document",
    "write-protected": "Write-protected document",
    "encrypted": "Password-encrypted (cannot open)",
    "empty": "Essentially empty document",
    # bug / edge buckets (from the corpus's own folder/file naming)
    "normalize-edge": "Normalization edge case",
    "bug-missing-id": "Missing IDs (e.g. table row id)",
    "bug-conflicting-id": "Conflicting IDs",
    "bug-cannot-normalize": "Known not-normalizable",
    "partial": "Partial / split content edge case",
}


def read(z, name):
    try:
        return z.read(name).decode("utf-8", "replace")
    except Exception:
        return ""


def classify(path):
    """Return a sorted list of tags for the .docx at `path`."""
    tags = set()
    try:
        z = zipfile.ZipFile(path)
    except Exception:
        return ["encrypted"]
    names = z.namelist()
    if not any(n.endswith("word/document.xml") for n in names):
        return ["encrypted"]

    # ---- part-name based ----
    def has(pred):
        return any(pred(n) for n in names)

    if has(lambda n: re.search(r"word/media/|word/.*image\d+\.(png|jpe?g|gif|bmp|tif)", n, re.I)) or has(
        lambda n: re.search(r"image\d+\.(png|jpe?g|gif|bmp|tif)$", n, re.I)
    ):
        tags.add("images")
    if has(lambda n: re.search(r"\.(wmf|emf)$", n, re.I)):
        tags.add("wmf-emf")
    if has(lambda n: "charts/chart" in n or re.search(r"chart\d*\.xml$", n)):
        tags.add("chart")
    if has(lambda n: "oleObject" in n or "Worksheet" in n or n.endswith(".bin")):
        tags.add("ole")
    if has(lambda n: "diagrams/" in n or re.search(r"word/(data|layout|colors|quickStyle)\d+\.xml$", n)):
        tags.add("smartart")
    if has(lambda n: n.startswith("customXml/") or re.search(r"customXml/item\d+\.xml$", n)):
        tags.add("custom-xml")
    if has(lambda n: re.search(r"word/header\d*\.xml$", n) or re.search(r"word/footer\d*\.xml$", n)):
        tags.add("headers-footers")
    if has(lambda n: n.endswith("word/numbering.xml")):
        tags.add("numbering-part")
    if has(lambda n: n.endswith("word/footnotes.xml")):
        # only count if there are real footnotes (not just separators)
        fx = read(z, "word/footnotes.xml")
        if 'w:type="separator"' in fx or "footnoteReference" in read(z, "word/document.xml"):
            if fx.count("<w:footnote ") > 2 or "footnoteReference" in read(z, "word/document.xml"):
                tags.add("footnotes")
    if has(lambda n: n.endswith("word/endnotes.xml")):
        if "endnoteReference" in read(z, "word/document.xml"):
            tags.add("endnotes")

    # ---- element scan over the body + headers/footers ----
    body = read(z, "word/document.xml")
    hf = "".join(
        read(z, n) for n in names if re.search(r"word/(header|footer)\d*\.xml$", n)
    )
    settings = read(z, "word/settings.xml")
    blob = body + hf

    def el(*needles):
        return any(s in blob for s in needles)

    if "commentReference" in body:
        tags.add("comments")
    if "<w:ins" in body or "<w:del" in body or "w:delText" in body:
        tags.add("tracked-changes")
    if "<w:tbl" in body:
        tags.add("tables")
    if "w:gridSpan" in body or "w:vMerge" in body:
        tags.add("merged-cells")
    if "<w:hyperlink" in body:
        tags.add("hyperlinks")
    if "w:bookmarkStart" in body:
        tags.add("bookmarks")
    if "w:fldChar" in body or "w:fldSimple" in body or "w:instrText" in body:
        tags.add("fields")
    if re.search(r"instrText[^<]*TOC", body) or "fldSimple" in body and "TOC" in body:
        tags.add("toc")
    if "w:numPr" in body:
        tags.add("lists")
    if el("<v:shape", "<v:rect", "<v:group", "v:imagedata"):
        tags.add("vml")
    if "txbxContent" in blob:
        tags.add("textbox")
    if "<m:oMath" in body:
        tags.add("math")
    if "<w:sdt" in body:
        tags.add("sdt")
    if "<w:smartTag" in body:
        tags.add("smarttag")
    if "<w:sym" in body:
        tags.add("symbols")
    if "w:pgBorders" in body:
        tags.add("page-borders")
    if 'w:orient="landscape"' in body:
        tags.add("landscape")
    if "w:bidi" in body:
        tags.add("rtl")
    if "w:shd" in body:
        tags.add("shading")
    if "w:drawing" in body:
        tags.add("drawing")
    # multi-column section
    for m in re.findall(r'<w:cols\b[^>]*w:num="(\d+)"', body):
        if int(m) > 1:
            tags.add("multi-column")
    if body.count("<w:sectPr") > 1:
        tags.add("section-breaks")
    if "w:titlePg" in body:
        tags.add("title-page")
    if "evenAndOddHeaders" in settings:
        tags.add("even-odd")
    if "background" in body and ("PowerPlusWaterMark" in blob or "WordPictureWatermark" in blob) or "<w:background" in body:
        tags.add("watermark")
    if "documentProtection" in settings:
        tags.add("protected")
    if "writeProtection" in settings:
        tags.add("write-protected")

    # essentially empty?
    if body.count("<w:p") <= 2 and "<w:tbl" not in body and "<w:r" not in body:
        tags.add("empty")

    return sorted(tags)


def name_tags(rel_dir, name):
    """Bug/edge tags inferred from the corpus's own folder/file naming."""
    t = set()
    low = (rel_dir + "/" + name).lower()
    if rel_dir.split("/")[0].lower() == "normalize":
        t.add("normalize-edge")
    if "miss" in low:
        t.add("bug-missing-id")
    if "conflicting" in low:
        t.add("bug-conflicting-id")
    if "unable to normalized" in low:
        t.add("bug-cannot-normalize")
    if "partial" in low or "splitpara" in low.replace(" ", ""):
        t.add("partial")
    return t


def main():
    if not os.path.isdir(SRC):
        raise SystemExit(f"corpus source not found: {SRC}")
    if os.path.isdir(FILES):
        shutil.rmtree(FILES)
    os.makedirs(FILES, exist_ok=True)

    entries = []
    tag_counts = {}
    cat_counts = {}
    for root, _dirs, fnames in os.walk(SRC):
        for fn in fnames:
            if not fn.lower().endswith(".docx"):
                continue
            spath = os.path.join(root, fn)
            rel = os.path.relpath(spath, SRC).replace("\\", "/")
            rel_dir = os.path.dirname(rel)
            category = rel.split("/")[0] if "/" in rel else "misc"

            # copy into the repo, preserving structure
            dpath = os.path.join(FILES, rel.replace("/", os.sep))
            os.makedirs(os.path.dirname(dpath), exist_ok=True)
            shutil.copy2(spath, dpath)

            tags = set(classify(spath)) | name_tags(rel_dir, fn)
            entries.append(
                {
                    "name": fn,
                    "path": "files/" + rel,
                    "folder": rel_dir,
                    "category": category,
                    "tags": sorted(tags),
                    "size": os.path.getsize(spath),
                }
            )
            cat_counts[category] = cat_counts.get(category, 0) + 1
            for t in tags:
                tag_counts[t] = tag_counts.get(t, 0) + 1

    entries.sort(key=lambda e: (e["category"], e["folder"], e["name"]))
    manifest = {
        "root": "files",
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

    print(f"classified {len(entries)} files into {len(cat_counts)} categories, {len(tag_counts)} tags")
    print("top tags:", ", ".join(f"{t}({tag_counts[t]})" for t in list(manifest['tags'])[:12]))


if __name__ == "__main__":
    main()
