# Mermaid flowchart-quality — manual test plan

Manual verification plan for the mermaid-fence-to-Word-drawing feature (branch
`claude/mermaid-flowchart-quality`). This plan is for a human running Word and
VS Code by hand — it is not automated.

## Setup

1. Build the extension package and the release CLI (from the `docxy` repo root):
   ```
   cd offxy-vscode && npm run package   # produces offxy-0.3.0.vsix
   cargo build --release -p docxy       # produces target/release/docxy.exe
   ```
2. **The extension version stays `0.3.0`** across this change (see
   `offxy-vscode/package.json`), so VS Code will not auto-detect a "new"
   version. Force a reinstall and reload:
   ```
   code --install-extension offxy-vscode/offxy-0.3.0.vsix --force
   ```
   then **Developer: Reload Window** from the command palette. Without the
   `--force` + reload, VS Code will keep running the previously-loaded
   extension host code.
3. Sample fixtures live in `offxy-vscode/samples/mermaid/*.md` (seven titled
   markdown docs, one mermaid fence each). Their converted `.docx` files live
   alongside them (`samples/mermaid/*.docx`) — these are gitignored build
   artifacts; if missing or stale, regenerate with:
   ```
   for f in offxy-vscode/samples/mermaid/*.md; do
     target/release/docxy.exe "$f" --docx "${f%.md}.docx"
   done
   ```

## How to view a diagram

Three equally-valid ways to look at the same rendered diagram — use whichever
fits the check:

1. **Word drawing tab** — open `samples/mermaid/NN-*.docx` directly (double-
   click in VS Code's Explorer, or `code samples/mermaid/NN-*.docx`). It opens
   in the **Docxy** custom editor and shows the diagram as native Word shapes
   (`wps:wsp` elements) inside the document flow.
2. **Webview inline SVG** — open `samples/mermaid/NN-*.md`, right-click the
   tab → **Reopen Editor With… → Docxy Markdown**. The mermaid fence renders
   as inline SVG in the webview, independent of the Word-shape codepath.
3. **Real Microsoft Word** — open the same `.docx` in desktop Word (or Word
   Online) to confirm the shapes survive round-tripping through an actual
   OOXML consumer, not just Offxy's own renderer.

## Per-feature checks

| # | Feature | Fixture | PASS criteria |
|---|---|---|---|
| 1 | Elbow connectors | `01-basic-flow.md` | Arrows route in orthogonal (elbow) segments, not straight diagonals; no connector visually cuts through a box it isn't connected to. `Start → Approved? → Provision/Reject → Notify user` all readable in flow order. |
| 2 | classDef / style / `:::` colors | `02-colors.md` | `A[Request]` (class `ok`) shows green fill/stroke as declared by `classDef ok`. `C[Deploy]`'s explicit `style C stroke:#990000` visibly overrides whatever stroke color the `warn` class would otherwise apply — the direct `style` statement wins. |
| 3 | Nested subgraphs | `03-subgraphs.md` | Two top-level containers (`Shared VPC`, `Workload VPC`) draw as visibly separate labeled boxes; `Domain core` draws as a smaller labeled box nested *inside* `Shared VPC`, containing `Desktop Lifecycle` and `Org and Entitlement`. All three container titles are fully visible and not clipped at the top edge. |
| 4 | Crossing reduction | `04-crossing.md` | The `A→X, B→Y, A→Y, B→X, X→Z, Y→Z` fan-in/fan-out layout keeps edge crossings to a practical minimum — verify visually that the layout isn't a naive top-to-bottom stack with edges crossing needlessly. |
| 5 | **Word vs. webview parity** | any of 01–04 | Open the same diagram both as `.docx` (Docxy editor) and as `.md` (Docxy Markdown webview). Confirm they match: same box positions/labels, same fill/stroke colors, same elbow polylines, same subgraph container boxes. They are two independent renderers reading the same parsed diagram model — divergence between them is a bug. |

## Known limitations to verify are still gaps (not regressions)

These are empirically confirmed **current scope boundaries**, checked against
real-world diagrams copied verbatim from an unrelated spec doc
(`2026-07-20-vdi-domain-model-design.md`). Re-verify they are still present —
if any of these silently start working, that's good news worth noting, not a
problem:

- **`&` multi-target fan-out collapses** (`10-aliaksei-context-map.md`).
  Source lines like `IA -->|claims| DL & POOL & OE & CAT & TKT & OBS & REP &
  PV & DI & NB & AI` do **not** produce 11 edges into the 11 already-existing
  real nodes. Instead the parser emits **one extra phantom node** whose label
  is the raw, un-split `DL & POOL & OE & CAT & TKT & OBS & REP & PV & DI & NB
  & AI` string, with a single edge from `IA` into that phantom node. Confirmed
  by inspecting `word/document.xml`: node count is 14 real domain nodes + 6
  phantom "join-list" nodes (one per fan-out edge in the source), not 14 real
  nodes with 6 edges each fanning out individually.
- **`{{hexagon}}` shape** (`11-aliaksei-topology.md`). The `EB{{EventBridge
  bus...}}` node does not render as a hexagon. It renders as a `diamond`
  (decision) preset shape, and the label loses one layer of the `{{ }}`
  delimiter, showing as `{EventBridge bus cross-account / cross-region +
  Schema Registry}` (single braces still present in the label text) instead
  of the plain label. Track this as: hexagon syntax falls back to decision-
  diamond handling, with an incomplete strip of the delimiter.
- **Edges pointing at a subgraph ID** (both `03-subgraphs.md`'s `EFF -->
  Work` and `11-aliaksei-topology.md`'s `SharedVPC <-->|...| EB` / `EB <-->|
  ...| WorkloadVPC`). Real mermaid treats an edge whose target is a subgraph
  ID as connecting to the subgraph's boundary. This engine instead emits a
  **separate phantom node** carrying the subgraph's bare ID as its label
  (e.g. a floating box literally reading "Work", "SharedVPC", or
  "WorkloadVPC") in addition to the correctly-drawn, correctly-titled
  subgraph container. Cosmetic but confirm it's still there.
- **`:::className` after a chained arrow can be misparsed as an edge label**
  (`02-colors.md`). `A[Request]:::ok` on its own definition line applies the
  class correctly (verified: green fill/stroke). But `B --> C[Deploy]:::warn`
  and `C --> D[Done]:::ok`, where the classed node is introduced mid-arrow-
  chain, do **not** apply the class to the node — instead a phantom edge-
  label shape appears reading `::warn` / `::ok` (one leading colon dropped),
  and `C`/`D` keep the default node color. Worth re-checking specifically
  since it's a subtler variant of the `:::` feature than the classDef test
  in row 2 above already covers.
- **Sequence diagrams are best-effort only** (`12-aliaksei-provisioning-
  seq.md`). There is no lifeline/activation-bar rendering — participants
  become plain flowchart-style boxes and each message becomes a directed,
  labeled edge in call order. This actually reads reasonably well for a
  linear happy path (all 6 participants appeared as distinct boxes, all 14
  messages appeared as edges with correct, readable label text, in the
  correct order). The known rough edge: `alt` / `else` / `end` fragment
  keywords are not modelled as fragment boxes — the `alt automated mode
  (platform has rights)` guard text is silently dropped, while the `else
  manual mode (no rights → human performs)` guard text leaks out as a
  floating, disconnected phantom node reading "no rights → human performs".
  Treat sequence-diagram support as "message order + text" fidelity only, not
  "sequence diagram" fidelity (no lifelines, inconsistent alt/else handling).

## Round-trip check

Every generated `.docx` embeds the original mermaid source as accessibility
alt-text on the drawing, so the fence text survives the md→docx conversion
losslessly even where the visual rendering has gaps above. Verify with:
```
unzip -p samples/mermaid/NN-*.docx word/document.xml | grep -o 'descr="mermaid:[^"]*"'
```
Confirmed present (one match, matching the original fence) in all seven
`samples/mermaid/*.docx` produced for this test plan, including the three
real-world Aliaksei fixtures.
