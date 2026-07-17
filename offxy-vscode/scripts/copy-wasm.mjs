// Copy the freshly built wasm artifact into the extension's media/ folder so the
// webview can load it. Run after `cargo build -p docxwasm --target
// wasm32-unknown-unknown --release` (see the `build:wasm` npm script).
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const src = resolve(here, '../../target/wasm32-unknown-unknown/release/docxwasm.wasm');
const dstDir = resolve(here, '../media');
const dst = resolve(dstDir, 'docxwasm.wasm');

if (!existsSync(src)) {
  console.error(`wasm artifact not found at ${src}\nBuild it first: cargo build -p docxwasm --target wasm32-unknown-unknown --release`);
  process.exit(1);
}
mkdirSync(dstDir, { recursive: true });
copyFileSync(src, dst);
console.log(`copied ${src} -> ${dst}`);
