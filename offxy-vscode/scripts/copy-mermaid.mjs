// Copy the vendored mermaid UMD bundle into the extension's media/ folder so
// the docx/markdown webview can load it as a plain <script> tag (no bundler
// needed — mermaid.min.js is a UMD build that defines `window.mermaid`). Run
// as part of `npm run build` (see the `build` npm script) after
// `npm install` has populated node_modules/mermaid.
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const dstDir = resolve(here, '../media');
mkdirSync(dstDir, { recursive: true });

const src = resolve(here, '../node_modules/mermaid/dist/mermaid.min.js');
const dst = resolve(dstDir, 'mermaid.min.js');
if (!existsSync(src)) {
  console.error(
    `mermaid.min.js not found at ${src}\n` +
      `Install it first: npm install (mermaid is a devDependency).`
  );
  process.exit(1);
}
copyFileSync(src, dst);
console.log(`copied ${src} -> ${dst}`);
