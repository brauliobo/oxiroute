import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests/browser',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 2,
  timeout: 30_000,
  expect: {
    timeout: 5_000,
  },
  reporter: process.env.CI
    ? [['line'], ['html', { outputFolder: 'playwright-report', open: 'never' }]]
    : 'list',
  use: {
    baseURL: 'http://127.0.0.1:4173',
    serviceWorkers: 'block',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    video: 'off',
  },
  projects: [
    {
      name: 'desktop-chromium',
      testIgnore: '**/stats/**',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: 'mobile-chromium',
      testIgnore: '**/stats/**',
      use: {
        ...devices['Pixel 5'],
      },
    },
    {
      name: 'stats-chromium',
      testMatch: '**/stats/**/*.spec.ts',
      use: {
        ...devices['Desktop Chrome'],
        baseURL: 'http://127.0.0.1:4174',
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: 'stats-mobile-chromium',
      testMatch: '**/stats/**/*.spec.ts',
      use: {
        ...devices['Pixel 5'],
        baseURL: 'http://127.0.0.1:4174',
      },
    },
  ],
  webServer: [
    {
      command: 'pnpm exec vite preview --host 127.0.0.1 --port 4173',
      url: 'http://127.0.0.1:4173',
      reuseExistingServer: false,
      timeout: 120_000,
    },
    {
      command: 'pnpm exec vite preview --config vite.stats.config.ts --base / --host 127.0.0.1 --port 4174',
      url: 'http://127.0.0.1:4174/stats.html',
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
})
