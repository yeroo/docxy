// #547: View > Ruler shows the suite's rulers in the editable HTML editor:
// markers at the caret paragraph's rendered indents, draggable with 1/8"
// snapping, correct at every zoom, themed, with a vertical ruler pinned to
// the surface's left edge. The fixture (rulers.docx) has "First indent"
// (left 1440, firstLine 720) at p0 and filler paragraphs after it.
import { bundleCopy, expect, openBundle, test } from './helpers.mjs';

async function showRulers(page) {
  await page.locator('.rtab[data-tab="View"]').click();
  await page.locator('#ribbon [data-act="ToggleRuler"]').click();
}

// Marker positions relative to the page's rect, with the rect-derived scale
// and the page padding — the same quantities the page's drawRulers uses.
async function markerDeltas(page) {
  return page.evaluate(() => {
    const pageEl = document.getElementById('page');
    const r = pageEl.getBoundingClientRect();
    const cs = getComputedStyle(pageEl);
    const left = (sel) => {
      const r = document.querySelector(sel).getBoundingClientRect();
      return r.left + r.width / 2;
    };
    return {
      scale: r.width / pageEl.offsetWidth,
      padL: parseFloat(cs.paddingLeft),
      w: r.width,
      padR: parseFloat(cs.paddingRight),
      left: left('.ruler-m-left') - r.left,
      first: left('.ruler-m-first') - r.left,
      right: left('.ruler-m-right') - r.left,
    };
  });
}

function near(got, want, what) {
  expect(Math.abs(got - want), `${what}: ${got} vs ${want}`).toBeLessThanOrEqual(2);
}

test('View > Ruler toggles both rulers and shows the button checked', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  const button = page.locator('#ribbon [data-act="ToggleRuler"]');
  await expect(page.locator('#ruler-h')).toBeHidden();
  await expect(page.locator('#ruler-v')).toBeHidden();

  await showRulers(page);
  await expect(page.locator('#ruler-h')).toBeVisible();
  await expect(page.locator('#ruler-v')).toBeVisible();
  await expect(button).toHaveClass(/checked/);

  await button.click();
  await expect(page.locator('#ruler-h')).toBeHidden();
  await expect(page.locator('#ruler-v')).toBeHidden();
  await expect(button).not.toHaveClass(/checked/);
  await guard.check();
});

test('markers sit at the caret paragraph rendered indents at zoom 1 and 2', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="0"]').click(); // "First indent": left 1440, firstLine 720.

  for (const zoomIn of [0, 10]) {
    for (let i = 0; i < zoomIn; i++) await page.locator('#zoom-in').click();
    const m = await markerDeltas(page);
    // px = twips * zoom / 15, margins included; scale is the rect-derived zoom.
    near(m.left, (m.padL + 1440 / 15) * m.scale, 'left marker');
    near(m.first, (m.padL + 2160 / 15) * m.scale, 'first-line marker');
    near(m.right, m.w - m.padR * m.scale, 'right marker');
  }
  await guard.check();
});

test('markers follow a pure caret move to the hanging-indent paragraph', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="1"]').click(); // "Hanging indent": left 1440, hanging 720, right 1440.
  await expect.poll(async () => {
    const m = await markerDeltas(page);
    return Math.abs(m.left - (m.padL + 1440 / 15) * m.scale);
  }, { message: 'left marker' }).toBeLessThanOrEqual(2);
  await expect.poll(async () => {
    const m = await markerDeltas(page);
    return Math.abs(m.first - (m.padL + 720 / 15) * m.scale);
  }, { message: 'first-line marker at left + hanging' }).toBeLessThanOrEqual(2);
  await expect.poll(async () => {
    const m = await markerDeltas(page);
    return Math.abs(m.right - (m.w - (m.padR + 1440 / 15) * m.scale));
  }, { message: 'right marker pulled in by the right indent' }).toBeLessThanOrEqual(2);
  await guard.check();
});

test('a list paragraph shows the synthetic indent markers', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="2"]').click();
  await page.locator('.rtab[data-tab="Home"]').click();
  await page.locator('#ribbon [data-act="Bullets"]').click();
  // paraHtml: a list paragraph with no ind gets left 360 and first -360.
  await expect(page.locator('p[data-p="2"]')).toHaveAttribute('style', /margin-left:18pt/);
  await expect(page.locator('p[data-p="2"]')).toHaveAttribute('style', /text-indent:-18pt/);

  await expect.poll(async () => {
    const m = await markerDeltas(page);
    return Math.abs(m.left - (m.padL + 360 / 15) * m.scale);
  }, { message: 'left marker at the synthetic list indent' }).toBeLessThanOrEqual(2);
  await expect.poll(async () => {
    const m = await markerDeltas(page);
    return Math.abs(m.first - (m.padL + 0) * m.scale);
  }, { message: 'first-line marker at the text origin (left + first = 0)' }).toBeLessThanOrEqual(2);
  await guard.check();
});

test('dragging the left marker snaps to the grid; one undo restores it', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="0"]').click();
  const box = (await page.locator('.ruler-m-left').boundingBox());
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 30, y, { steps: 5 });
  await page.mouse.up();
  // +30px at zoom 1 is +450 twips; snap(1440 + 450) lands on 1980 (99pt).
  await expect(page.locator('p[data-p="0"]')).toHaveAttribute('style', /margin-left:99pt/);

  await page.keyboard.press('Control+z');
  await expect(page.locator('p[data-p="0"]')).toHaveAttribute('style', /margin-left:72pt/);
  await guard.check();
});

test('the left square is draggable where first is 0', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="2"]').click(); // plain paragraph: first == left.
  const box = (await page.locator('.ruler-m-left').boundingBox());
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 30, y, { steps: 5 });
  await page.mouse.up();
  // Bottom half of the strip drags the LEFT marker (the suite's hruler_hit
  // row split), not the first-line triangle that shares its x.
  // snap(0 + 450) lands on 540 (27pt); first stays 0 and is not written.
  await expect(page.locator('p[data-p="2"]')).toHaveAttribute('style', /margin-left:27pt/);
  await expect(page.locator('p[data-p="2"]')).not.toHaveAttribute('style', /text-indent/);
  await guard.check();
});

test('clicking a marker without moving is not an edit', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  await page.locator('p[data-p="0"]').click();
  const box = (await page.locator('.ruler-m-left').boundingBox());
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width / 2, y);
  await page.mouse.down();
  await page.mouse.up();
  // No movement: no command, so the document is still clean and the
  // on-grid indent is untouched.
  const chip = await page.locator('#doc-chip').textContent();
  expect(chip).not.toContain('•');
  await expect(page.locator('p[data-p="0"]')).toHaveAttribute('style', /margin-left:72pt/);
  await guard.check();
});

test('the rulers follow the theme', async ({ page, guard }, testInfo) => {
  await openBundle(page, bundleCopy(testInfo, 'rulers.docx'));
  await showRulers(page);
  const bg = () => page.locator('#ruler-h').evaluate((n) => getComputedStyle(n).backgroundColor);
  const colors = () => page.evaluate(() => ({
    band: getComputedStyle(document.querySelector('.ruler-band')).backgroundColor,
    marker: getComputedStyle(document.querySelector('.ruler-m-left')).backgroundColor,
  }));
  const light = await bg();
  const darkButton = page.locator('#ribbon [data-act="DarkMode"]');
  await darkButton.click(); // auto -> light
  await darkButton.click(); // light -> dark
  expect(await bg()).not.toEqual(light);
  // Markers stay dark on the white content band in dark mode, as the
  // suite's ruler_colors keeps them near-black in both themes.
  const dark = await colors();
  expect(dark.marker).not.toEqual(dark.band);
  await guard.check();
});
