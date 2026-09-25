// AC3: edits made in the browser come back out of the saved file as Word.
import { readFileSync } from 'node:fs';
import {
  beforePayload, bundleCopy, docxy, documentXmlOfBundle, expect, openBundle, saveViaDownload, test, zipPart,
} from './helpers.mjs';

async function typeAtDocumentEnd(page, text) {
  await page.locator('#doc').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.type(text);
}

async function selectLeft(page, count) {
  for (let i = 0; i < count; i++) await page.keyboard.press('Shift+ArrowLeft');
}

test('typing and Bold survive a save and come back as Word', async ({ page, guard }, testInfo) => {
  const file = bundleCopy(testInfo);
  const original = readFileSync(file, 'utf8');
  await openBundle(page, file);

  await typeAtDocumentEnd(page, ' Hello browser');
  await selectLeft(page, 'Hello browser'.length);
  await page.locator('#rb-b').click();
  await expect(page.locator('#doc span', { hasText: 'Hello browser' })).toHaveCSS('font-weight', '700');
  // The selection survives the ribbon click, and the button lights with the
  // caret inside bold text (the suite's act_active rule).
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('ArrowLeft');
  await expect(page.locator('#rb-b')).toHaveClass(/checked/);
  await expect(page.locator('#doc-chip')).toContainText('•');

  const saved = await saveViaDownload(page, testInfo);
  expect(saved.suggested).toBe('sample.docx.html');
  await expect(page.locator('#doc-chip')).not.toContainText('•');

  // The file is the page rebuilt around a new package: byte-identical before
  // the payload block (engine, UI, template), as htmlbundle::rewrap makes it.
  const html = readFileSync(saved.path, 'utf8');
  expect(beforePayload(html)).toBe(beforePayload(original));

  const xml = documentXmlOfBundle(saved.path, testInfo);
  expect(xml).toMatch(/<w:r><w:rPr>(?:(?!<\/w:rPr>).)*<w:b\/>(?:(?!<\/w:rPr>).)*<\/w:rPr><w:t[^>]*>Hello browser<\/w:t>/);
  expect(docxy(saved.path, '--md', testInfo.outputPath('back.md'))).toContain('wrote');
  expect(readFileSync(testInfo.outputPath('back.md'), 'utf8')).toContain('**Hello browser**');

  await guard.check();
});

test('a saved page opens and saves again', async ({ page, guard }, testInfo) => {
  const file = bundleCopy(testInfo);
  await openBundle(page, file);
  await typeAtDocumentEnd(page, ' first');
  const first = await saveViaDownload(page, testInfo, 'first.docx.html');

  await page.goto('about:blank');
  await openBundle(page, first.path);
  await expect(page.locator('#doc')).toContainText('first');
  await typeAtDocumentEnd(page, ' second');
  const second = await saveViaDownload(page, testInfo, 'second.docx.html');

  const xml = documentXmlOfBundle(second.path, testInfo);
  expect(xml).toContain(' first');
  expect(xml).toContain(' second');
  await guard.check();
});

test('Download sample.docx gives the Word file with the edits', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await typeAtDocumentEnd(page, ' via backstage');
  await page.locator('.rtab.file').click();
  await expect(page.locator('#backstage')).toBeVisible();
  await page.locator('#bs-saveas').click();
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.locator('#saveas-docx').click(),
  ]);
  expect(download.suggestedFilename()).toBe('sample.docx');
  const path = testInfo.outputPath('sample.docx');
  await download.saveAs(path);
  expect(zipPart(readFileSync(path), 'word/document.xml')).toContain(' via backstage');
  // The page itself still has unsaved edits: only a .docx was downloaded.
  await expect(page.locator('#doc-chip')).toContainText('•');
  await guard.check();
});

test('astral text after a link and a tracked change lands at the right offset', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'review.docx'));
  const para = page.locator('p[data-p="1"]');
  await expect(para).toContainText('Start linkaddedremoved end.');
  await para.click();
  await page.keyboard.press('End');
  await page.keyboard.insertText('X\u{1F600}Y');
  // Step back over "Y" (one code point, one UTF-16 unit) and type between.
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.insertText('Z');
  const saved = await saveViaDownload(page, testInfo);
  const xml = documentXmlOfBundle(saved.path, testInfo);
  expect(xml).toContain('end.X\u{1F600}ZY');
  // The tracked changes and the link are untouched.
  expect(xml).toContain('<w:ins w:id="1" w:author="Ada"');
  expect(xml).toContain('<w:delText>removed</w:delText>');
  expect(xml).toMatch(/<w:hyperlink r:id="rId5">/);
  await guard.check();
});
