// Copy the freshly built wasm artifacts into the extension's media/ folder so
// the webviews can load them. Run after the cargo builds of the `build:wasm`
// npm script.
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const dstDir = resolve(here, '../media');
mkdirSync(dstDir, { recursive: true });

// The PDF engine (#2259) is the desktop suite's renderer: a member of the suite
// workspace (PdfCraft needs a newer Rust than the root's MSRV), built with its
// `wasm` profile.
const SOURCES = [
  ['docxwasm', '../../target/wasm32-unknown-unknown/release', 'cargo build -p docxwasm --target wasm32-unknown-unknown --release'],
  ['gridwasm', '../../target/wasm32-unknown-unknown/release', 'cargo build -p gridwasm --target wasm32-unknown-unknown --release'],
  [
    'pdfpages',
    '../../suite/target/wasm32-unknown-unknown/wasm',
    'cargo build --manifest-path suite/Cargo.toml -p pdfpages --lib --target wasm32-unknown-unknown --profile wasm',
  ],
];

for (const [name, dir, build] of SOURCES) {
  const src = resolve(here, `${dir}/${name}.wasm`);
  const dst = resolve(dstDir, `${name}.wasm`);
  if (!existsSync(src)) {
    console.error(`wasm artifact not found at ${src}\nBuild it first: ${build}`);
    process.exit(1);
  }
  copyFileSync(src, dst);
  console.log(`copied ${src} -> ${dst}`);
}
