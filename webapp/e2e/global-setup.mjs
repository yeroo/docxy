// Export every fixture docx as editable HTML with a real docxy, into a temp
// directory the specs read through DOCXY_E2E (a JSON file of paths).
//
// DOCXY_BIN names a docxy built with `--features html-export` (CI builds it in
// its own step). Without it, this builds one locally (debug) first.
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdtempSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const exe = process.platform === 'win32' ? '.exe' : '';

function docxyBin() {
  if (process.env.DOCXY_BIN) {
    const bin = resolve(process.env.DOCXY_BIN);
    for (const candidate of [bin, bin + exe]) if (existsSync(candidate)) return candidate;
    throw new Error(`DOCXY_BIN=${process.env.DOCXY_BIN} does not exist; build it with ` +
      '`cargo build -p docxy --features html-export`');
  }
  execFileSync('cargo', ['build', '-p', 'docxy', '--features', 'html-export'], {
    cwd: root,
    stdio: 'inherit',
  });
  return join(root, 'target', 'debug', 'docxy' + exe);
}

export default async function globalSetup() {
  const bin = docxyBin();
  const dir = mkdtempSync(join(tmpdir(), 'docxy-e2e-'));
  const fixtures = join(here, '..', 'fixtures');
  const out = { bin, dir, bundles: {}, sources: {} };
  for (const name of readdirSync(fixtures).filter((f) => f.endsWith('.docx'))) {
    const src = join(dir, name);
    copyFileSync(join(fixtures, name), src);
    const bundle = src + '.html';
    execFileSync(bin, [src, '--html', bundle], { stdio: 'pipe' });
    out.bundles[name] = bundle;
    out.sources[name] = src;
  }
  const handoff = join(dir, 'fixtures.json');
  writeFileSync(handoff, JSON.stringify(out, null, 2));
  process.env.DOCXY_E2E = handoff;
}
