import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { unlock } from './support'
import { mockCameras } from './support/cameras'

const THEMES = ['light', 'dark'] as const

async function violations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze()
  return results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)
}

for (const theme of THEMES) {
  test.describe(`cameras ${theme} theme`, () => {
    test.beforeEach(async ({ page }) => {
      await page.addInitScript((value) => localStorage.setItem('oxiroute.theme', value), theme)
      await mockCameras(page)
      await page.goto('/#/cameras')
      await unlock(page)
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    })

    test('the streams grid, with its degraded, idle and offline tiles, has no accessibility violations', async ({ page }) => {
      await expect(page.locator('[data-camera]')).toHaveCount(6)
      expect(await violations(page)).toEqual([])
    })

    test('an expanded viewer list, a running preview and an action result have no accessibility violations', async ({ page }) => {
      await page.getByRole('button', { name: '3 viewers' }).click()
      await page.getByRole('button', { name: 'Preview live / cam-front' }).click()
      await expect(page.locator('[data-camera="cam-front"] video')).toBeVisible()
      await page.getByRole('button', { name: 'Start recording live / cam-front with archive' }).click()
      await expect(page.locator('[data-feedback] [role="status"]')).toContainText('was requested')
      expect(await violations(page)).toEqual([])
    })

    test('the drop publisher dialog has no accessibility violations', async ({ page }) => {
      await page.getByRole('button', { name: 'Drop publisher of live / cam-front' }).click()
      await expect(page.getByRole('dialog', { name: 'Drop the publisher of live / cam-front' })).toBeVisible()
      expect(await violations(page)).toEqual([])
    })

    for (const tab of ['Recordings', 'Access keys']) {
      test(`${tab} has no accessibility violations`, async ({ page }) => {
        await page.getByRole('tab', { name: tab }).click()
        await expect(page.getByRole('tab', { name: tab })).toHaveAttribute('aria-selected', 'true')
        await expect(page.getByRole('tabpanel').locator('table').first()).toBeVisible()
        expect(await violations(page)).toEqual([])
      })
    }
  })
}
