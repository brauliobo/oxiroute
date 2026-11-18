import { expect, test } from '@playwright/test'

import { dashboardResponse, installApiMock, requestPath, unlock } from './support'

test.describe('console shell', () => {
  test.beforeEach(async ({ page }) => {
    await installApiMock(page, (request) => dashboardResponse(requestPath(request)))
    await page.goto('/#/overview')
    await unlock(page)
    await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
  })

  test('has one main landmark, one h1 and a title that follows the view', async ({ page }) => {
    await expect(page.locator('main')).toHaveCount(1)
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('OxiRoute')
    await expect(page).toHaveTitle('Runtime observatory - OxiRoute')
    await page.getByRole('link', { name: 'Operations' }).click()
    await expect(page).toHaveTitle('Operations - OxiRoute')
    await page.getByRole('link', { name: 'Audit' }).click()
    await expect(page).toHaveTitle('Durable audit history - OxiRoute')
  })

  test('is reachable from the keyboard: skip link first, then the navigation in order', async ({ page }) => {
    await page.reload()
    await page.keyboard.press('Tab')
    const skip = page.getByRole('link', { name: 'Skip to content' })
    await expect(skip).toBeFocused()
    await expect(skip).toBeVisible()
    await page.keyboard.press('Enter')
    await expect(page.locator('main')).toBeFocused()

    await page.reload()
    const labels: string[] = []
    for (let step = 0; step < 14; step += 1) {
      await page.keyboard.press('Tab')
      labels.push(await page.evaluate(() => document.activeElement?.textContent?.trim() ?? ''))
    }
    const links = ['Overview', 'Statistics', 'Operations', 'Certificates', 'Events', 'Audit', 'Configuration', 'Provenance']
    const positions = links.map((name) => labels.indexOf(name))
    expect(positions.every((position) => position >= 0)).toBe(true)
    expect([...positions].sort((a, b) => a - b)).toEqual(positions)
  })

  test('moves focus to the content when a link is followed with the keyboard', async ({ page }) => {
    await page.getByRole('link', { name: 'Events' }).focus()
    await page.keyboard.press('Enter')
    await expect(page.locator('main')).toBeFocused()
    await expect(page.getByRole('link', { name: 'Events' })).toHaveAttribute('aria-current', 'page')
  })

  test('switches and remembers the theme and is comfortable', async ({ page }) => {
    await expect(page.locator('html')).toHaveAttribute('data-density', 'comfortable')
    await page.getByLabel('Theme').selectOption('light')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(244, 245, 247)')
    await page.reload()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    await page.getByLabel('Theme').selectOption('dark')
    await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(13, 17, 23)')
    const height = await page.getByRole('button', { name: 'Sign out' }).evaluate((button) => button.getBoundingClientRect().height)
    expect(height).toBeGreaterThanOrEqual(44)
  })

  test('follows the system scheme when the theme is automatic', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'light' })
    await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(244, 245, 247)')
    await page.emulateMedia({ colorScheme: 'dark' })
    await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(13, 17, 23)')
  })
})
