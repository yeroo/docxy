// Copy the freshly built wasm artifacts into the extension's media/ folder so
// the webviews can load them. Run after `cargo build -p docxwasm -p gridwasm
// --target wasm32-unknown-unknown --release` (see the `build:wasm` npm
// script).
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const dstDir = resolve(here, '../media');
mkdirSync(dstDir, { recursive: true });

for (const name of ['docxwasm', 'gridwasm']) {
  const src = resolve(here, `../../target/wasm32-unknown-unknown/release/${name}.wasm`);
  const dst = resolve(dstDir, `${name}.wasm`);
  if (!existsSync(src)) {
    console.error(`wasm artifact not found at ${src}\nBuild it first: cargo build -p ${name} --target wasm32-unknown-unknown --release`);
    process.exit(1);
  }
  copyFileSync(src, dst);
  console.log(`copied ${src} -> ${dst}`);
}
