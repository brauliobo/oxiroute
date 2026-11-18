import { defineConfig, devices } from '@playwright/test'
import { origins, ports } from './tests/browser/ports'

export default defineConfig({
  testDir: './tests/browser',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 2,
  timeout: 30_000,
  expect: {
    timeout: 5_000,
    toHaveScreenshot: { animations: 'disabled', maxDiffPixelRatio: 0.05 },
  },
  reporter: process.env.CI
    ? [['line'], ['html', { outputFolder: 'playwright-report', open: 'never' }]]
    : 'list',
  use: {
    baseURL: origins.console,
    serviceWorkers: 'block',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    video: 'off',
  },
  projects: [
    {
      name: 'desktop-chromium',
      testIgnore: ['**/stats/**', '**/gallery/**'],
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: 'mobile-chromium',
      testIgnore: ['**/stats/**', '**/gallery/**'],
      use: {
        ...devices['Pixel 5'],
      },
    },
    {
      name: 'stats-chromium',
      testMatch: '**/stats/**/*.spec.ts',
      use: {
        ...devices['Desktop Chrome'],
        baseURL: origins.stats,
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: 'stats-mobile-chromium',
      testMatch: '**/stats/**/*.spec.ts',
      use: {
        ...devices['Pixel 5'],
        baseURL: origins.stats,
      },
    },
    {
      name: 'gallery-chromium',
      testMatch: '**/gallery/**/*.spec.ts',
      use: {
        ...devices['Desktop Chrome'],
        baseURL: origins.gallery,
        viewport: { width: 1100, height: 900 },
      },
    },
  ],
  webServer: [
    {
      command: `pnpm exec vite preview --host 127.0.0.1 --port ${ports.console}`,
      url: origins.console,
      reuseExistingServer: false,
      timeout: 120_000,
    },
    {
      command: `pnpm exec vite preview --config vite.stats.config.ts --base / --host 127.0.0.1 --port ${ports.stats}`,
      url: `${origins.stats}/stats.html`,
      reuseExistingServer: false,
      timeout: 120_000,
    },
    {
      command: `pnpm exec vite --host 127.0.0.1 --port ${ports.gallery}`,
      url: `${origins.gallery}/gallery.html`,
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
})
