import { expect, test } from '@playwright/test'

import { json, requestPath } from '../support'
import { installConfigApi, openConfiguration, selectObject, themes, type ConfigApiHandler } from './support'

const serverAddress = {
  code: 'E_INVALID_UPSTREAM_SERVER',
  severity: 'error',
  stage: 'validation',
  path: '/upstream_pools/origins/servers/server-1/endpoint.address',
  message: 'The upstream server address is invalid.',
  detail: 'expected a socket address such as 192.0.2.10:8080',
}

const unplaced = {
  code: 'E_INVALID_FIELD',
  severity: 'error',
  stage: 'parse',
  path: '/config',
  message: 'the config field does not match the canonical configuration schema',
  detail: 'invalid value, expected socket address',
}

const rejectValidation = (diagnostics: unknown[]): ConfigApiHandler => (request) =>
  requestPath(request) === '/api/v1/config/validate' ? json({ diagnostics }, 422) : undefined

for (const theme of themes) {
  test.describe(`validation diagnostics (${theme})`, () => {
    test('shows the message and path beside the offending field and announces the result', async ({ page }) => {
      await installConfigApi(page, rejectValidation([serverAddress, unplaced]))
      await openConfiguration(page, theme)
      await selectObject(page, 'Upstream pools / origins')
      await page.locator('[data-field="upstream_pools[].servers[].endpoint.address"] input').fill('not an address')

      await page.getByRole('button', { name: 'Validate candidate' }).click()

      const field = page.locator('[data-field="upstream_pools[].servers[].endpoint.address"]')
      await expect(field).toContainText('The upstream server address is invalid.')
      await expect(field).toContainText('expected a socket address such as 192.0.2.10:8080')
      await expect(field).toContainText('/upstream_pools/origins/servers/server-1/endpoint.address')
      const input = field.locator('input')
      await expect(input).toHaveAttribute('aria-invalid', 'true')
      await expect(input).toHaveAccessibleDescription(/The upstream server address is invalid\./)
      await expect(page.locator('.diagnostic-summary')).toHaveText('2 errors')
      await expect(page.locator('#diagnostics-heading')).toBeFocused()
      await expect(page.locator('[role="alert"].ox-sr-only')).toHaveText('Validation finished: 2 errors.')
      await page.screenshot({ path: `/tmp/g1-cfg/validation-${theme}.png`, fullPage: true })
    })

    test('keeps an unplaceable diagnostic in the list and jumps from a placed one to its field', async ({ page }) => {
      await installConfigApi(page, rejectValidation([serverAddress, unplaced]))
      await openConfiguration(page, theme)
      await selectObject(page, 'Upstream pools / origins')
      await page.getByRole('button', { name: 'Validate candidate' }).click()

      const list = page.locator('.diagnostic-list')
      await expect(list).toContainText('invalid value, expected socket address')
      await expect(list.getByRole('button')).toHaveCount(1)

      await selectObject(page, 'Cache stores / memory-responses')
      await list.getByRole('button', { name: /The upstream server address is invalid/ }).click()
      await expect(page.locator('[data-field="upstream_pools[].servers[].endpoint.address"] input')).toBeFocused()
    })
  })
}
