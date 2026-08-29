// Pin the extension-host routing contract for tracked-change review verbs.
// The live wasm behavior is covered in docxwasm; this test catches the VS Code
// integration failure mode where a new verb exists in MCP/Rust but is omitted
// from DOCX_CTL (or accidentally treated as read-only/mutating).

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, '..', 'src', 'extension.ts'), 'utf8');
const webview = readFileSync(join(here, '..', 'media', 'webview.js'), 'utf8');
const contract = source.match(/const DOCX_CTL:[\s\S]*?= \{([\s\S]*?)\n\};\n\nconst EDITORS/);
assert.ok(contract, 'extension.ts must declare DOCX_CTL before EDITORS');

const withoutComments = contract[1]
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/\/\/.*$/gm, '');
const wasmBlock = withoutComments.match(/wasmVerbs:\s*new Set\(\[([\s\S]*?)\]\),\s*mutatingVerbs:/);
const mutatingBlock = withoutComments.match(/mutatingVerbs:\s*new Set\(\[([\s\S]*?)\]\),/);
assert.ok(wasmBlock, 'DOCX_CTL must declare wasmVerbs');
assert.ok(mutatingBlock, 'DOCX_CTL must declare mutatingVerbs');

const quoted = (block) => [...block.matchAll(/'([^']+)'/g)].map((match) => match[1]);
const wasm = new Set(quoted(wasmBlock[1]));
const mutating = new Set(quoted(mutatingBlock[1]));

const readOrNavigate = [
  'doc.revisions',
  'doc.revision-current',
  'doc.revision-next',
  'doc.revision-previous',
];
const actions = [
  'doc.revision-accept',
  'doc.revision-reject',
  'doc.revisions-accept-all',
  'doc.revisions-reject-all',
];

for (const verb of [...readOrNavigate, ...actions]) {
  assert.ok(wasm.has(verb), `${verb} must route to the live DOCX wasm session`);
}
for (const verb of readOrNavigate) {
  assert.ok(!mutating.has(verb), `${verb} must not dirty the VS Code document`);
}
for (const verb of actions) {
  assert.ok(mutating.has(verb), `${verb} must register VS Code dirty/undo state`);
}

// Pin production makeCtlHost itself: it must feed the computed repaint bit to
// requestCtl and include the two navigation commands whose caret change is
// visible. The set-driven assertions below then cover all four action verbs.
const callWasmRoute = source.match(
  /callWasm: \(requestJson: string\) => \{([\s\S]*?)return document\.requestCtl\(requestJson, repaint\);/,
);
assert.ok(callWasmRoute, 'makeCtlHost.callWasm must pass its repaint decision to requestCtl');
assert.match(callWasmRoute[1], /this\.spec\.ctl\.mutatingVerbs\.has\(verb\)/);
assert.match(callWasmRoute[1], /verb === 'doc\.revision-next'/);
assert.match(callWasmRoute[1], /verb === 'doc\.revision-previous'/);

const repaints = (verb) =>
  mutating.has(verb) || verb === 'doc.revision-next' || verb === 'doc.revision-previous';
for (const verb of [...actions, 'doc.revision-next', 'doc.revision-previous']) {
  assert.ok(repaints(verb), `${verb} must repaint the active tab`);
}
for (const verb of ['doc.revisions', 'doc.revision-current']) {
  assert.ok(!repaints(verb), `${verb} must remain a non-repainting inspection`);
}

// The default onMutated bucket is the production undo/redo adapter used by
// all four review actions. Assert it replays the wasm stack in both directions
// using the reported step count, rather than merely trusting set membership.
const undoBucket = source.match(
  /const steps = undoSteps \?\? 1;([\s\S]*?)\n\s*}\n\n\s*\/\*\* Drive one host-orchestrated inverse/,
);
assert.ok(undoBucket, 'makeCtlHost.onMutated must retain its wasm undo bucket');
assert.match(undoBucket[1], /i < steps/);
assert.match(undoBucket[1], /op: 'undo'/);
assert.match(undoBucket[1], /op: 'redo'/);

// A protected/no-op wasm command returns commandApplied:false. Pin the real
// webview choke point so it cannot turn that denial into a false VS Code
// dirty marker and dead undo entry merely because the opcode is mutating.
const userCmdRoute = webview.match(/function userCmd\(str\) \{([\s\S]*?)\n  \}/);
assert.ok(userCmdRoute, 'webview.js must retain the userCmd edit-event choke point');
assert.match(userCmdRoute[1], /const view = cmd\(str\)/);
assert.match(
  userCmdRoute[1],
  /MUTATING\.has\(op\) && view\.commandApplied === true/,
  'userCmd must emit a VS Code edit only when wasm confirms the command applied',
);

console.log('docx routing: 8 tracked-review verbs have the expected VS Code route, repaint, and undo classification');
