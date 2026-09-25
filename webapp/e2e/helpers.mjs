// Shared fixtures for the browser specs: the exported bundles, a page that is
// guarded against network use, CSP violations and script errors, and small
// helpers to read .docx and bundle files in node.
import { test as base, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { copyFileSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { inflateRawSync } from 'node:zlib';

export { expect };

export function env() {
  return JSON.parse(readFileSync(process.env.DOCXY_E2E, 'utf8'));
}

// A private copy of a fixture's bundle for one test (tests save over files).
export function bundleCopy(testInfo, name = 'sample.docx') {
  const e = env();
  const dest = testInfo.outputPath(name + '.html');
  copyFileSync(e.bundles[name], dest);
  return dest;
}

export const test = base.extend({
  // Every page: only file: loads, CSP violations and errors recorded, and
  // (for the fallback path) no File System Access API when asked.
  guard: async ({ page, context }, use) => {
    const blocked = [];
    const errors = [];
    await context.route('**/*', (route) => {
      const url = route.request().url();
      if (url.startsWith('file:')) return route.continue();
      blocked.push(url);
      return route.abort();
    });
    page.on('pageerror', (e) => errors.push('pageerror: ' + e.message));
    page.on('console', (m) => { if (m.type() === 'error') errors.push('console: ' + m.text()); });
    await page.addInitScript(() => {
      window.__cspViolations = [];
      document.addEventListener('securitypolicyviolation', (e) => {
        window.__cspViolations.push(e.violatedDirective + ' ' + e.blockedURI);
      });
    });
    const g = {
      blocked,
      errors,
      async check() {
        const csp = await page.evaluate(() => window.__cspViolations);
        expect(csp, 'CSP violations').toEqual([]);
        expect(blocked, 'network requests').toEqual([]);
        expect(errors, 'page errors').toEqual([]);
      },
    };
    await use(g);
  },
});

// Open a bundle and wait until the editor is up.
export async function openBundle(page, file, { noPicker = true } = {}) {
  if (noPicker) {
    // Force the download fallback (Chromium would open a native dialog).
    await page.addInitScript(() => {
      Object.defineProperty(window, 'showSaveFilePicker', { value: undefined, configurable: true });
    });
  }
  await page.goto(pathToFileURL(file).href);
  await page.waitForSelector('body[data-ready="true"]', { timeout: 20_000 });
}

// Save through the page (Ctrl+S) and return the downloaded file's path.
export async function saveViaDownload(page, testInfo, name) {
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.keyboard.press('Control+s'),
  ]);
  const path = testInfo.outputPath(name || download.suggestedFilename());
  await download.saveAs(path);
  return { path, suggested: download.suggestedFilename() };
}

// Run the real docxy on a file (`--docx` / `--md` headless conversions).
export function docxy(...args) {
  return execFileSync(env().bin, args, { stdio: 'pipe' }).toString();
}

// Read one part out of a ZIP (enough of the format for a .docx).
export function zipPart(bytes, name) {
  const buf = Buffer.from(bytes);
  let eocd = buf.length - 22;
  while (eocd >= 0 && buf.readUInt32LE(eocd) !== 0x06054b50) eocd--;
  if (eocd < 0) throw new Error('not a zip');
  const count = buf.readUInt16LE(eocd + 10);
  let at = buf.readUInt32LE(eocd + 16);
  for (let i = 0; i < count; i++) {
    const method = buf.readUInt16LE(at + 10);
    const size = buf.readUInt32LE(at + 20);
    const nameLen = buf.readUInt16LE(at + 28);
    const extraLen = buf.readUInt16LE(at + 30);
    const commentLen = buf.readUInt16LE(at + 32);
    const local = buf.readUInt32LE(at + 42);
    const entry = buf.toString('utf8', at + 46, at + 46 + nameLen);
    if (entry === name) {
      const lNameLen = buf.readUInt16LE(local + 26);
      const lExtraLen = buf.readUInt16LE(local + 28);
      const data = buf.subarray(local + 30 + lNameLen + lExtraLen, local + 30 + lNameLen + lExtraLen + size);
      return method === 0 ? data.toString('utf8') : inflateRawSync(data).toString('utf8');
    }
    at += 46 + nameLen + extraLen + commentLen;
  }
  throw new Error(name + ' not in zip');
}

// The Word document inside a bundle, via the real docxy (`--docx`).
export function documentXmlOfBundle(bundlePath, testInfo) {
  const out = testInfo.outputPath('extracted-' + Date.now() + '.docx');
  docxy(bundlePath, '--docx', out);
  return zipPart(readFileSync(out), 'word/document.xml');
}

const PAYLOAD = '<script type="application/x-docxy-payload" id="docxy-payload">';
// Everything before the payload block: the engine, UI and template.
export function beforePayload(html) {
  return html.slice(0, html.lastIndexOf(PAYLOAD));
}

export function outputDir(testInfo) {
  return join(testInfo.outputDir);
}
