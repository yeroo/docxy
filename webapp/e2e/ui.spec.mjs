// AC8: the page is the suite's window, driven from the suite's own snapshot.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  bundleCopy, documentXmlOfBundle, expect, openBundle, saveViaDownload, test,
} from './helpers.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const snapshot = JSON.parse(readFileSync(join(here, '..', '..', 'htmlbundle', 'web', 'ribbon-docx.json'), 'utf8'));

// What a tab should render, in order: [groups, acts, commands(id, icon)].
function expected(tab) {
  const groups = tab.groups.map((g) => g.title);
  const acts = [];
  const cmds = [];
  const cmd = (c) => { acts.push(c.act); cmds.push([c.id, c.icon]); };
  for (const g of tab.groups) {
    for (const c of g.items) {
      if (c.cmd) cmd(c.cmd);
      (c.cmds || []).forEach(cmd);
      if (c.primary) cmd(c.primary);
      (c.menu || []).forEach(cmd);
      (c.rows || []).forEach((row) => row.forEach((cell) => cmd(cell.cmd)));
      (c.kind === 'gallery' ? c.items : []).forEach((it) => acts.push(it.act));
    }
    if (g.launcher) acts.push(g.launcher);
  }
  return { groups, acts, cmds };
}

async function rendered(page) {
  return page.evaluate(() => {
    const r = document.getElementById('ribbon');
    return {
      groups: [...r.querySelectorAll('.rgroup')].map((g) => g.dataset.title),
      acts: [...r.querySelectorAll('[data-act]')].map((b) => b.dataset.act),
      cmds: [...r.querySelectorAll('[data-cmd]')].map((b) => b.dataset.cmd),
      combos: [...r.querySelectorAll('.combo')].map((b) => b.dataset.cmd),
      // Each button's drawn icon, reduced to its path data (combo boxes show
      // their value instead, as in the suite).
      icons: Object.fromEntries([...r.querySelectorAll('.rb[data-cmd]')].map((b) => {
        const svg = b.querySelector('svg');
        return [b.dataset.cmd, svg ? [...svg.querySelectorAll('path')].map((p) => p.getAttribute('d')).join('|') : null];
      })),
    };
  });
}

function iconPaths(svgText) {
  return [...svgText.matchAll(/<path[^>]*\sd="([^"]*)"/g)].map((m) => m[1]).join('|');
}

test('every ribbon tab matches the suite snapshot: groups, acts and icons', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  const names = await page.locator('.rtab').allTextContents();
  expect(names).toEqual(snapshot.tabs.map((t) => t.name));
  for (const tab of snapshot.tabs.filter((t) => t.kind === 'ribbon')) {
    await page.locator(`.rtab[data-tab="${tab.name}"]`).click();
    const want = expected(tab);
    const got = await rendered(page);
    expect(got.groups, tab.name).toEqual(want.groups);
    expect(got.acts, tab.name).toEqual(want.acts);
    expect(got.cmds, tab.name).toEqual(want.cmds.map((c) => c[0]));
    for (const [id, icon] of want.cmds) {
      if (got.combos.includes(id)) continue;
      expect(got.icons[id], `${tab.name}/${id} icon`).toBe(iconPaths(snapshot.icons[icon]));
    }
  }
  const qat = await page.locator('.qat-btn').evaluateAll((bs) => bs.map((b) => b.id));
  expect(qat).toEqual(snapshot.qat.map((q) => q.id));
  await guard.check();
});

test('the Table tab appears only with the caret in a table', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await expect(page.locator('.rtab.contextual')).toHaveCount(0);
  await page.locator('p[data-p="17.1.0.0"]').click();
  const tableTab = page.locator('.rtab.contextual');
  await expect(tableTab).toHaveText('Table');
  await tableTab.click();
  const want = expected(snapshot.contextual[0]);
  expect((await rendered(page)).acts).toEqual(want.acts);
  // Table editing is not in the browser yet: drawn dimmed, saying so.
  const row = page.locator('#rb-rowabove');
  await expect(row).toHaveClass(/unsupported/);
  await expect(row).toHaveAttribute('aria-disabled', 'true');
  // Leaving the table drops the tab and returns to Home.
  await page.locator('p[data-p="4"]').click();
  await expect(page.locator('.rtab.contextual')).toHaveCount(0);
  await expect(page.locator('.rtab.active')).toHaveText('Home');
  await guard.check();
});

test('Alt shows key tips, and a key tip runs its command', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await page.locator('p[data-p="4"]').click();
  await page.keyboard.press('Alt');
  const tabTips = page.locator('.rtab .keytip');
  await expect(tabTips).toHaveText(snapshot.tabs.map((t) => t.keyTip));
  await page.keyboard.press('h');
  await expect(page.locator('#rb-b .keytip')).toHaveText('1');
  await page.keyboard.press('Escape');
  await expect(page.locator('.keytip')).toHaveCount(0);
  // Alt, H, 1 = Bold on the selection.
  await page.keyboard.press('Control+a');
  await page.keyboard.press('Alt');
  await page.keyboard.press('h');
  await page.keyboard.press('1');
  await expect(page.locator('.keytip')).toHaveCount(0);
  await expect(page.locator('#doc-chip')).toContainText('•');
  await guard.check();
});

test('hovering shows the screen tip, and unsupported commands say why', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await page.locator('#rb-b').hover();
  const tip = page.locator('#screentip');
  await expect(tip).toContainText('Bold');
  await expect(tip).toContainText('Ctrl+B');
  await page.locator('.rtab[data-tab="Insert"]').click();
  const table = page.locator('#rb-table');
  await expect(table).toHaveClass(/unsupported/);
  await table.hover();
  await expect(tip).toContainText('Not available in the browser yet');
  await table.click({ force: true });
  await expect(page.locator('#doc-chip')).not.toContainText('•');
  await guard.check();
});

test('themes follow prefers-color-scheme and the toggle', async ({ page, guard }, testInfo) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await openBundle(page, bundleCopy(testInfo));
  const theme = () => page.evaluate(() => ({
    mode: document.documentElement.dataset.theme,
    bg: getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
    label: document.getElementById('theme-btn').textContent,
  }));
  expect(await theme()).toEqual({ mode: 'dark', bg: snapshot.theme.dark.background, label: '◑ Auto' });
  await page.emulateMedia({ colorScheme: 'light' });
  await expect.poll(async () => (await theme()).mode).toBe('light');
  expect((await theme()).bg).toBe(snapshot.theme.light.background);
  await page.locator('#theme-btn').click();
  expect(await theme()).toMatchObject({ mode: 'light', label: '☀ Light' });
  await page.locator('#theme-btn').click();
  expect(await theme()).toMatchObject({ mode: 'dark', label: '☽ Dark' });
  await page.locator('#theme-btn').click();
  expect(await theme()).toMatchObject({ mode: 'light', label: '◑ Auto' });
  await guard.check();
});

test('the Backstage rail is the suite rail', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await page.locator('.rtab.file').click();
  await expect(page.locator('.rail-item')).toHaveText(snapshot.backstage.map((b) => b.label));
  for (const id of ['bs-new', 'bs-open', 'bs-close']) await expect(page.locator('#' + id)).toBeDisabled();
  await expect(page.locator('#bs-pane')).toContainText('sample.docx');
  await page.locator('#bs-back').click();
  await expect(page.locator('#backstage')).toBeHidden();
  await expect(page.locator('#doc')).toBeVisible();
  await guard.check();
});

test('closing with unsaved edits asks first; after a save it does not', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await page.locator('p[data-p="4"]').click();
  await page.keyboard.type('x');
  const dialogs = [];
  page.on('dialog', (d) => { dialogs.push(d.type()); d.dismiss(); });
  await page.close({ runBeforeUnload: true });
  await expect.poll(() => dialogs).toEqual(['beforeunload']);

  const page2 = await page.context().newPage();
  await openBundle(page2, bundleCopy(testInfo));
  await page2.locator('p[data-p="4"]').click();
  await page2.keyboard.type('y');
  await saveViaDownload(page2, testInfo);
  const later = [];
  page2.on('dialog', (d) => { later.push(d.type()); d.dismiss(); });
  await page2.close({ runBeforeUnload: true });
  await new Promise((r) => setTimeout(r, 300));
  expect(later).toEqual([]);
  expect(guard.blocked).toEqual([]);
});

test('IME composition inserts its text once', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'review.docx'));
  await page.locator('p[data-p="0"]').click();
  await page.keyboard.press('End');
  // What an IME does: start, draw into the DOM itself, then finish.
  await page.evaluate(() => {
    const doc = document.getElementById('doc');
    doc.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' }));
    const sel = document.getSelection();
    const r = sel.getRangeAt(0);
    r.insertNode(document.createTextNode('かな'));
    doc.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: 'かな' }));
  });
  await expect(page.locator('p[data-p="0"]')).toHaveText('Review fixtureかな');
  const saved = await saveViaDownload(page, testInfo);
  const xml = documentXmlOfBundle(saved.path, testInfo);
  expect(xml.split('かな').length - 1).toBe(1);
  await guard.check();
});

test('a real IME composition (Chromium) inserts its text once', async ({ page, guard, browserName }, testInfo) => {
  test.skip(browserName !== 'chromium', 'IME emulation is a Chromium DevTools feature');
  await openBundle(page, bundleCopy(testInfo, 'review.docx'));
  await page.locator('p[data-p="0"]').click();
  await page.keyboard.press('End');
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Input.imeSetComposition', { text: 'か', selectionStart: 1, selectionEnd: 1 });
  await cdp.send('Input.imeSetComposition', { text: 'かな', selectionStart: 2, selectionEnd: 2 });
  await cdp.send('Input.insertText', { text: 'かな' });
  await expect(page.locator('p[data-p="0"]')).toHaveText('Review fixtureかな');
  await guard.check();
});

test('drops, drags and spellcheck replacements are cancelled', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'review.docx'));
  await page.locator('p[data-p="0"]').click();
  const before = await page.locator('#doc').innerHTML();
  const prevented = await page.evaluate(() => ['insertReplacementText', 'insertFromDrop', 'deleteByDrag'].map((t) => {
    const e = new InputEvent('beforeinput', { inputType: t, data: 'zzz', cancelable: true, bubbles: true });
    document.getElementById('doc').dispatchEvent(e);
    return e.defaultPrevented;
  }));
  expect(prevented).toEqual([true, true, true]);
  expect(await page.locator('#doc').innerHTML()).toBe(before);
  await expect(page.locator('#doc-chip')).not.toContainText('•');
  await guard.check();
});

test('Find selects the next match; Replace all edits', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo));
  await page.locator('p[data-p="0"]').click();
  await page.keyboard.press('Control+f');
  await expect(page.locator('#findbar')).toBeVisible();
  await page.locator('#find-input').fill('rich TEXT');
  await page.keyboard.press('Enter');
  expect(await page.evaluate(() => document.getSelection().toString())).toBe('Rich text');
  // (Replace All runs docxcore's replace_all, which does not yet account for
  // tracked-change text in a paragraph; this document has none.)
  await page.locator('#find-input').fill('Rich text');
  await page.locator('#replace-input').fill('Rich prose');
  await page.getByRole('button', { name: 'Replace all' }).click();
  await expect(page.locator('#doc')).toContainText('Rich prose');
  await expect(page.locator('#doc')).not.toContainText('Rich text');
  await guard.check();
});
