# DOCX tracked-change inventory

This inventory records the revision markup present before tracked-change actions
were implemented and defines the model boundary used by the implementation plan.

## Existing fixture coverage

The packaged `.docx` fixtures under `docxcore/tests/fixtures`, `assets`, and the
editor integrations contain no WordprocessingML revision elements. The existing
in-source XML fixtures cover:

| Fixture/test | Revision form | Pre-project representation |
| --- | --- | --- |
| `docxcore/src/serialize.rs` tracked-change round-trip | `w:ins`, `w:del`, `w:delText` | Visible `Inline::Revision` with opaque wrapper XML |
| `docxcore/src/package.rs` section extraction test | `w:sectPrChange` | Preserved only inside opaque trailing `w:sectPr` XML |

Task 1 adds model fixtures for nested insert/delete wrappers, all six property
scopes, missing metadata, move/custom records, and a future unknown revision
kind. Task 6 remains responsible for adding realistic packaged `.docx` fixtures.

## Classification contract

| Classification | WordprocessingML forms | Review behavior |
| --- | --- | --- |
| Inline content | `w:ins`, `w:del` (including `w:delText`) | Supported accept/reject target |
| Property change | `w:rPrChange`, `w:pPrChange`, `w:tblPrChange`, `w:trPrChange`, `w:tcPrChange`, `w:sectPrChange` | Supported accept/reject target; current properties live on the owner and the prior property container lives on `PropertyChange` |
| Move revision | `w:moveFrom`, `w:moveTo`, their range start/end markers | Lossless, explicitly unsupported |
| Custom-XML revision | insert/delete/move custom-XML range start/end markers | Lossless, explicitly unsupported |
| Other unsupported revision | cell insert/delete/merge, conflict insert/delete, or a future producer-specific kind | Lossless, explicitly unsupported |

Every modeled record carries `RevisionMetadata`: a document-local stable target,
optional source id/author/date, and decoded unknown attributes. The complete raw
wrapper stays on the owning revision node, so unknown XML is retained exactly.
Property changes also retain a present, absent, or malformed prior snapshot.

## Addressing and order

`RevisionTarget` is assigned once after parsing and stored on the node. It is not
a block index or a structural path, so removing an earlier revision does not
invalidate a target. `Document::revisions` computes a fresh ordinal for display
and navigation each time while actions resolve the stable target. Enumeration is
source preorder (outer wrapper before nested content) and records parent/depth;
bulk transforms can therefore process greater depth first for deterministic
innermost-to-outermost behavior. Cloning preserves targets, metadata, snapshots,
nesting, and raw XML.
