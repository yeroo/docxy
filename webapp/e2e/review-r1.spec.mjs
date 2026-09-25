// Regressions from review round 1: saving while typing, word deletes over a
// selection, and which links Ctrl+click may open.
import { writeFileSync } from 'node:fs';
import {
  bundleCopy, documentXmlOfBundle, expect, openBundle, test,
} from './helpers.mjs';

// A File System Access stand-in whose writes wait until the test releases
// them, recording what each write received.
async function slowPicker(page) {
  await page.addInitScript(() => {
    window.__written = [];
    window.__pending = [];
    window.__opened = 0;
    window.showSaveFilePicker = async () => ({
      createWritable: async () => {
        window.__opened++;
        await new Promise((release) => window.__pending.push(release));
        return {
          write: async (data) => { window.__written.push(data); },
          close: async () => {},
        };
      },
    });
  });
}

const release = (page) => page.evaluate(() => window.__pending.shift()());

test('an edit typed while a save is being written stays unsaved', async ({ page, guard }, testInfo) => {
  await slowPicker(page);
  await openBundle(page, bundleCopy(testInfo), { noPicker: false });
  const chip = page.locator('#doc-chip');
  await page.locator('p[data-p="4"]').click();
  await page.keyboard.press('Home');
  await page.keyboard.type('EARLY ');
  await page.keyboard.press('Control+s');
  await expect.poll(() => page.evaluate(() => window.__opened)).toBe(1);

  // Typed while the write is in flight, and a second save asked for meanwhile.
  await page.keyboard.type('LATE ');
  await page.keyboard.press('Control+s');
  // The second save waits for the first: no second write is open yet.
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => window.__opened)).toBe(1);

  await release(page);
  await expect.poll(() => page.evaluate(() => window.__written.length)).toBe(1);
  // The first write did not include LATE, so the page must still say unsaved
  // until the queued save lands.
  await expect.poll(() => page.evaluate(() => window.__opened)).toBe(2);
  await expect(chip).toContainText('\u2022');

  await release(page);
  await expect.poll(() => page.evaluate(() => window.__written.length)).toBe(2);
  await expect(chip).not.toContainText('\u2022');

  const [first, second] = await page.evaluate(() => window.__written);
  const f1 = testInfo.outputPath('first.docx.html');
  const f2 = testInfo.outputPath('second.docx.html');
  writeFileSync(f1, first);
  writeFileSync(f2, second);
  const x1 = documentXmlOfBundle(f1, testInfo);
  const x2 = documentXmlOfBundle(f2, testInfo);
  expect(x1).toContain('EARLY ');
  expect(x1).not.toContain('LATE ');
  expect(x2).toContain('EARLY LATE ');
  await guard.check();
});

test('Ctrl+Backspace over a selection deletes just the selection', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  const para = page.locator('p[data-p="4"]');
  await expect(para).toContainText(/^Docxy opens real/);
  await para.click();
  // Select "opens" (the paragraph wraps, so Home would only reach its line).
  await page.evaluate(() => {
    const text = document.querySelector('p[data-p="4"] [data-o="0"]').firstChild;
    document.getSelection().setBaseAndExtent(text, 6, text, 11);
  });
  expect(await page.evaluate(() => document.getSelection().toString())).toBe('opens');
  await page.keyboard.press('Control+Backspace');
  await expect(para).toContainText(/^Docxy  real/);

  // Collapsed, it still deletes the word before the caret.
  await page.keyboard.press('Control+Backspace');
  await expect(para).toContainText(/^ real/);
  await guard.check();
});

test('Ctrl+click opens web and mail links only', async ({ page, guard }, testInfo) => {
  await page.addInitScript(() => {
    window.__opens = [];
    window.open = (url) => { window.__opens.push(url); return null; };
  });
  await openBundle(page, bundleCopy(testInfo));
  const link = page.locator('#doc .link').first();
  await expect(link).toHaveAttribute('title', 'https://github.com/yeroo/docxy');
  await link.click({ modifiers: ['Control'] });
  expect(await page.evaluate(() => window.__opens)).toEqual(['https://github.com/yeroo/docxy']);

  for (const target of ['javascript:alert(1)', 'data:text/html,x', 'file:///C:/x.html', '#bookmark', '']) {
    await link.evaluate((el, t) => el.setAttribute('title', t), target);
    await link.click({ modifiers: ['Control'] });
  }
  await link.evaluate((el) => el.setAttribute('title', 'mailto:a@example.com'));
  await link.click({ modifiers: ['Control'] });
  expect(await page.evaluate(() => window.__opens)).toEqual([
    'https://github.com/yeroo/docxy',
    'mailto:a@example.com',
  ]);
  await guard.check();
});
