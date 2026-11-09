import { expect, test } from '@playwright/test'

import { PAGE, serveStats } from './support'

const DARK_BACKGROUND = 'rgb(13, 17, 23)'
const LIGHT_BACKGROUND = 'rgb(244, 245, 247)'
const background = (page: import('@playwright/test').Page) =>
  page.evaluate(() => getComputedStyle(document.body).backgroundColor)

test.describe('stats page theme', () => {
  test('follows the system colour scheme by default', async ({ page }) => {
    await serveStats(page)
    for (const [scheme, expected] of [['dark', DARK_BACKGROUND], ['light', LIGHT_BACKGROUND]] as const) {
      await page.emulateMedia({ colorScheme: scheme })
      await page.goto(PAGE)
      await expect(page.getByLabel('Theme')).toHaveValue('auto')
      expect(await background(page)).toBe(expected)
      expect(await page.evaluate(() => document.documentElement.hasAttribute('data-theme'))).toBe(false)
    }
  })

  test('an explicit theme beats the system, shows in the URL and survives a reload', async ({ page }) => {
    await serveStats(page)
    await page.emulateMedia({ colorScheme: 'light' })
    await page.goto(PAGE)
    await page.getByLabel('Theme').selectOption('dark')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    await expect(page).toHaveURL(/theme=dark/)
    expect(await background(page)).toBe(DARK_BACKGROUND)
    await page.reload()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    await expect(page.getByLabel('Theme')).toHaveValue('dark')
    expect(await background(page)).toBe(DARK_BACKGROUND)
    await page.getByLabel('Theme').selectOption('auto')
    await expect(page.locator('html')).not.toHaveAttribute('data-theme', /.*/)
    expect(await background(page)).toBe(LIGHT_BACKGROUND)
    await expect(page).not.toHaveURL(/theme/)
  })

  test('forced light on a dark system stays light', async ({ page }) => {
    await serveStats(page)
    await page.emulateMedia({ colorScheme: 'dark' })
    await page.goto(`${PAGE}?theme=light`)
    expect(await background(page)).toBe(LIGHT_BACKGROUND)
  })

  test('keeps text readable with forced colours', async ({ page }) => {
    await serveStats(page)
    await page.emulateMedia({ forcedColors: 'active' })
    await page.goto(PAGE)
    await expect(page.getByRole('button', { name: 'Pause' })).toBeVisible()
    await expect(page.getByRole('search')).toBeVisible()
  })
})
