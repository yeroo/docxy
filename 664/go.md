IMPLEMENT plan v2 (.workbench/plan.md).

Your three notes are all accepted as part of the plan:
1. Where the body and the "Changes from v1" section disagree, the changes section wins (GridClip.spent + grid_clip_live; move option (b); cross-tab cut = translated copy; move_ref_expr behind move_refs, move_block_formula per moved cell).
2. The live-cut content check compares formula text, constant input, style and f_attrs, not the cached value.
3. Enter with a range selected and a live clip uses the whole selection as the paste area (tiling rule), like Ctrl+V.

Commit on this branch, run the Done commands from the plan, and reply IMPLEMENTED <sha> with the tests added, the commands run with their counts, and whether the uiharness cases ran (and if not, why).
