// parity.test.mjs — the committed JS<->Rust MCP tool-surface parity net.
//
// server.mjs's TOOLS array is a hand-maintained mirror of
// docxy/src/mcp.rs::tool_defs() + xlsxy/src/mcp.rs::tool_defs() (see the
// header comment atop server.mjs). Nothing enforces that mirror stays
// accurate short of a human re-reading three files on every change — three
// separate wave landings each deferred building an automated check for
// exactly that reason. This test is that check, landed.
//
// Design: rather than have this test talk to Rust directly (spawning cargo,
// parsing target/ output — slow and machine-dependent), both sides pin to
// ONE checked-in artifact, tools-expected.json:
//   - This file compares server.mjs's live TOOLS array against the JSON
//     file (deep, key-order-independent — object key order is an
//     implementation detail, not a contract).
//   - docxy/src/mcp.rs and xlsxy/src/mcp.rs each carry a Rust test
//     (`tool_defs_matches_committed_mcp_parity_snapshot`) comparing their
//     own tool_defs() against the SAME file (filtered to their own
//     "docxy_"/"xlsxy_" name prefix).
// A change to either side that isn't mirrored on the other fails somewhere
// committed: change Rust without updating the snapshot -> the Rust test
// fails; change server.mjs without updating the snapshot -> this test
// fails; update the snapshot without updating the other side -> that
// side's test fails. All three must move together.
//
// Regenerating the snapshot after a real tool_defs() change: see the doc
// comment on docxy/src/mcp.rs's `dump_tool_defs_json_for_mcp_parity_snapshot`
// test (env-var-gated JSON dump, one invocation per crate) — merge the two
// dumped arrays (docxy's tools first) into tools-expected.json, then update
// server.mjs by hand to match, re-running this file to confirm.
//
// Run directly: `node mcp/parity.test.mjs` (also wired as `npm run
// test:mcp-parity` in package.json). No test framework dependency, per this
// extension's zero-runtime-dependency bundling policy — a plain assert-based
// script that exits non-zero on failure, like the rest of this repo's
// hand-rolled test style.

import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

import { TOOLS, DOCXY_VERBS, XLSXY_VERBS } from './server.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SNAPSHOT_PATH = path.join(__dirname, 'tools-expected.json');

function main() {
  const raw = fs.readFileSync(SNAPSHOT_PATH, 'utf8');
  const expected = JSON.parse(raw);

  assert.ok(Array.isArray(expected), `${SNAPSHOT_PATH} must be a JSON array`);
  assert.equal(
    expected.length,
    56,
    'the committed snapshot is expected to carry the wave-3 56-tool surface; if a tool ' +
      'was deliberately added/removed, regenerate the snapshot (see the header comment) ' +
      'rather than editing this number blindly',
  );

  // The main check: server.mjs's live TOOLS, deep-compared against the
  // checked-in snapshot generated from the Rust side. deepStrictEqual
  // ignores object key insertion order (irrelevant to JSON Schema
  // semantics) but is strict about array order, types, and every nested
  // value — exactly what a "did the tool surface drift" check needs.
  assert.deepStrictEqual(
    TOOLS,
    expected,
    `server.mjs's TOOLS drifted from the committed JS<->Rust MCP parity snapshot ` +
      `(${SNAPSHOT_PATH}). Either server.mjs's docxyToolDefs()/xlsxyToolDefs() changed ` +
      `without updating the snapshot, or vice versa — see this file's header comment for ` +
      `the regeneration recipe.`,
  );

  // Bonus coherence check, cheap given DOCXY_VERBS/XLSXY_VERBS are already
  // imported: every forwarding tool (i.e. not the specially-handled *_list/
  // *_new pair) must have exactly one verb-map entry, and vice versa —
  // mirrors docxy/xlsxy's own `verb_for_maps_every_tool_to_its_exact_spec_verb`
  // completeness check on the Rust side, so a tool renamed on one side but
  // not the other (or a stale verb-map entry for a removed tool) fails here
  // too, not just at runtime when an agent calls it.
  const namesWithPrefix = (prefix) =>
    TOOLS.filter((t) => t.name.startsWith(prefix)).map((t) => t.name);
  const checkVerbCoverage = (app, verbs, special) => {
    const forwarded = namesWithPrefix(`${app}_`).filter((n) => !special.includes(n));
    assert.deepStrictEqual(
      [...Object.keys(verbs)].sort(),
      [...forwarded].sort(),
      `${app.toUpperCase()}_VERBS must map exactly the forwarding ${app}_* tools ` +
        `(everything except ${special.join('/')})`,
    );
  };
  checkVerbCoverage('docxy', DOCXY_VERBS, ['docxy_list', 'docxy_new']);
  checkVerbCoverage('xlsxy', XLSXY_VERBS, ['xlsxy_list', 'xlsxy_new']);

  console.log(`mcp parity: ${TOOLS.length} tools match the committed snapshot (0 mismatches)`);
}

main();
