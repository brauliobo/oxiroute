import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { json, requestPath } from '../support'
import { installConfigApi, openConfiguration, selectObject, themes } from './support'

const invalid = [{
  code: 'E_INVALID_UPSTREAM_SERVER',
  severity: 'error',
  stage: 'validation',
  path: '/upstream_pools/origins/servers/server-1/endpoint.address',
  message: 'The upstream server address is invalid.',
  detail: 'expected a socket address such as 192.0.2.10:8080',
}, {
  code: 'E_INVALID_FIELD',
  severity: 'warning',
  stage: 'parse',
  path: '/config',
  message: 'the config field does not match the canonical configuration schema',
  detail: 'invalid value, expected socket address',
}]

async function violations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze()
  return results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)
}

for (const theme of themes) {
  test.describe(`configuration accessibility (${theme})`, () => {
    test('the loaded editor has no violations', async ({ page }) => {
      await installConfigApi(page)
      await openConfiguration(page, theme)
      expect(await violations(page)).toEqual([])
      await page.screenshot({ path: `/tmp/g1-cfg/general-${theme}.png`, fullPage: true })

      await selectObject(page, 'Upstream pools / origins')
      expect(await violations(page)).toEqual([])
      await page.screenshot({ path: `/tmp/g1-cfg/pool-${theme}.png`, fullPage: true })

      await selectObject(page, 'Listeners / forward-h3', 'FORWARD_HTTP3')
      expect(await violations(page)).toEqual([])
    })

    test('the invalid validation state has no violations', async ({ page }) => {
      await installConfigApi(page, (request) =>
        requestPath(request) === '/api/v1/config/validate' ? json({ diagnostics: invalid }, 422) : undefined)
      await openConfiguration(page, theme)
      await selectObject(page, 'Upstream pools / origins')
      await page.getByRole('button', { name: 'Validate candidate' }).click()
      await expect(page.locator('.field-error')).toBeVisible()
      expect(await violations(page)).toEqual([])
    })

    test('the save review dialog has no violations', async ({ page }) => {
      await installConfigApi(page)
      await openConfiguration(page, theme)
      await page.locator('[data-field="version"] input').fill('2')
      await page.getByRole('button', { name: 'Validate candidate' }).click()
      await page.getByRole('button', { name: 'Review save' }).click()
      await expect(page.getByRole('dialog', { name: 'Save review' })).toBeVisible()
      expect(await violations(page)).toEqual([])
    })

    test('the stale conflict banner and reapply report have no violations', async ({ page }) => {
      await installConfigApi(page, (request) =>
        requestPath(request) === '/api/v1/config' && request.method() === 'PUT'
          ? json({ error: { code: 'config_revision_conflict', message: 'stale' }, diskRevision: 'newer-disk-revision', diagnostics: [] }, 409)
          : undefined)
      await openConfiguration(page, theme)
      await page.locator('[data-field="version"] input').fill('2')
      await page.getByRole('button', { name: 'Validate candidate' }).click()
      await page.getByRole('button', { name: 'Review save' }).click()
      await page.getByRole('button', { name: 'Save canonical configuration' }).click()
      await expect(page.locator('.revision-banner.stale')).toBeVisible()
      expect(await violations(page)).toEqual([])
    })
  })
}
