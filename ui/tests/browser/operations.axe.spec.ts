import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { unlock } from './support'
import { mockOperations } from './support/operations'

const THEMES = ['light', 'dark'] as const
const TABS = ['Runtime', 'Listeners and pools', 'Servers', 'In-flight requests', 'Registrations'] as const

async function violations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze()
  return results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)
}

for (const theme of THEMES) {
  test.describe(`operations ${theme} theme`, () => {
    test.beforeEach(async ({ page }) => {
      await page.addInitScript((value) => localStorage.setItem('oxiroute.theme', value), theme)
      await mockOperations(page)
      await page.goto('/#/operations')
      await unlock(page)
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    })

    for (const tab of TABS) {
      test(`${tab} has no accessibility violations`, async ({ page }) => {
        await page.getByRole('tab', { name: tab }).click()
        await expect(page.getByRole('tab', { name: tab })).toHaveAttribute('aria-selected', 'true')
        await expect(page.getByRole('tabpanel')).not.toBeEmpty()
        expect(await violations(page)).toEqual([])
      })
    }

    test('an action outcome and its dialog have no accessibility violations', async ({ page }) => {
      await page.getByRole('tab', { name: 'Servers' }).click()
      await page.getByRole('button', { name: 'Drain server primary in pool web-backends' }).click()
      await expect(page.getByRole('dialog', { name: 'Drain server primary' })).toBeVisible()
      expect(await violations(page)).toEqual([])

      await page.getByRole('dialog').getByRole('button', { name: 'Drain', exact: true }).click()
      await expect(page.getByRole('status').filter({ hasText: 'is now draining' }).first()).toBeVisible()
      expect(await violations(page)).toEqual([])
    })
  })
}
