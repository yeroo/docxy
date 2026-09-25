// @visual: screenshot baselines of the docx window, light and dark.
//
// Windows only: the page draws the document's own fonts (Calibri, Cambria),
// which Linux runners do not have, and one baseline per platform would never
// agree. Regenerate on Windows with:
//   npx playwright test --grep @visual --update-snapshots
import { bundleCopy, expect, openBundle, test } from './helpers.mjs';

test.describe('@visual', () => {
  test.skip(process.platform !== 'win32', 'screenshot baselines are Windows-only (document fonts)');

  for (const scheme of ['light', 'dark']) {
    test(`the docx window, ${scheme}`, async ({ page, guard, browserName }, testInfo) => {
      test.skip(browserName !== 'chromium', 'one browser is enough for pixels');
      await page.emulateMedia({ colorScheme: scheme });
      await openBundle(page, bundleCopy(testInfo));
      await page.mouse.move(0, 0);
      await expect(page).toHaveScreenshot(`docx-${scheme}.png`);
      // The Backstage too.
      await page.locator('.rtab.file').click();
      await page.mouse.move(0, 0);
      // The export time changes with every export.
      await expect(page).toHaveScreenshot(`backstage-${scheme}.png`, { mask: [page.locator('#bs-exported')] });
      await guard.check();
    });
  }
});
