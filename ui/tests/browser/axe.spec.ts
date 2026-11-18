import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { unlock } from './support'
import { THEMES, installDemoDashboard } from './support/dashboard'

async function violations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze()
  return results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)
}

for (const theme of THEMES) {
  test.describe(`console accessibility, ${theme} theme`, () => {
    test.beforeEach(async ({ page }) => {
      await installDemoDashboard(page, theme)
    })

    test('Overview with the topology inspector open has no automatically detectable violations', async ({ page }) => {
      await page.goto('/#/overview')
      await unlock(page)
      await expect(page.getByRole('heading', { name: 'Upstream pools' })).toBeVisible()
      await expect(page.getByRole('heading', { name: 'Connected RTMP clients' })).toBeVisible()
      await expect(page.getByText('client-session-0001')).toBeVisible()
      expect(await violations(page)).toEqual([])

      await page.locator('[data-node-id^="upstream_pool"]').click()
      await expect(page.getByLabel('Topology node inspector')).toBeVisible()
      expect(await violations(page)).toEqual([])
    })

    test('Statistics has no automatically detectable violations', async ({ page }) => {
      await page.goto('/#/stats')
      await unlock(page)
      await expect(page.getByRole('heading', { name: 'Upstream pools' })).toBeVisible()
      await expect(page.getByText('ejected-origin')).toBeVisible()
      expect(await violations(page)).toEqual([])
    })
  })
}
