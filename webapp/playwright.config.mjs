// Browser tests for docxy's editable HTML (htmlbundle/web), in Chromium and
// Firefox. globalSetup exports the fixtures with a real docxy built with
// `--features html-export` (DOCXY_BIN, or a local debug build).
//
// Screenshot baselines (@visual) are Windows-only: they are checked where the
// document fonts (Calibri, Cambria) exist, and skipped elsewhere.
import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.mjs',
  timeout: 60_000,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    viewport: { width: 1280, height: 800 },
    deviceScaleFactor: 1,
    acceptDownloads: true,
    trace: 'retain-on-failure',
  },
  expect: {
    toHaveScreenshot: { maxDiffPixelRatio: 0.01, animations: 'disabled' },
  },
  // One baseline per test and theme, not per platform: they only run on Windows.
  snapshotPathTemplate: '{testDir}/__screenshots__/{projectName}/{arg}{ext}',
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'firefox', use: { browserName: 'firefox' } },
  ],
});
