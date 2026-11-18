import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

import { PAGE, serveStats } from './support'

for (const theme of ['light', 'dark'] as const) {
  test.describe(`stats page accessibility, ${theme} theme`, () => {
    test('has no automatically detectable violations', async ({ page }) => {
      await serveStats(page)
      await page.goto(`${PAGE}?theme=${theme}`)
      await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'live')
      await expect(page.getByRole('heading', { name: 'Pools and servers' })).toBeVisible()
      const results = await new AxeBuilder({ page }).analyze()
      expect(results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)).toEqual([])
    })
  })
}
