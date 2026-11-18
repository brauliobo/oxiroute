import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { MANAGEMENT_TOKEN, installApiMock, json, managementMonitoring, requestPath, unlock } from './support'
import { THEMES, expectNoAxeViolations, expectNoPageScroll, trackExternalRequests, useTheme } from './support/lists'
import { importReportResponse } from '../../src/test/importFixtures'

interface Requested {
  path: string
  method: string
  authorization: string | undefined
}

async function mockReports(page: Page, blocked = false): Promise<Requested[]> {
  const selectedReport = importReportResponse(blocked)
  const inventory = structuredClone(selectedReport)
  inventory.selection = null
  inventory.report = null
  inventory.preview = null
  const requested: Requested[] = []
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    requested.push({ path, method: request.method(), authorization: request.headers()['authorization'] })
    if (path === '/api/v1/monitoring') return managementMonitoring()
    if (path === '/api/v1/import-reports' && request.method() === 'GET') return json(inventory)
    if (path === '/api/v1/import-reports/0' && request.method() === 'GET') return json(selectedReport)
    return undefined
  })
  return requested
}

test.describe('provenance workspace browser gates', () => {
  test('browses redacted native import reports without crossing the offline read-only boundary', async ({ page }) => {
    const external = trackExternalRequests(page)
    const requested = await mockReports(page)
    await page.goto('/#/provenance')
    await unlock(page)

    const main = page.getByRole('main')
    await expect(main.getByRole('heading', { name: 'apache import: apache-static-reverse-proxy' })).toBeVisible()
    await expect(main).toContainText('Finalized')
    await expect(main).toContainText('1 report retained')
    await expect(main.getByRole('table', { name: 'Source files' })).toContainText('Source 1')
    await expect(main.getByRole('table', { name: 'Source dependencies' })).toContainText('Source 2')
    await expect(main.getByRole('table', { name: 'Canonical field origins' })).toContainText('/http_services/0/routes/0/action')
    await expect(main.getByRole('heading', { name: 'Requirements' })).toBeVisible()
    await expect(main.getByText('2 retained from the deployment and activation requirements.')).toBeVisible()
    await expect(main.locator('pre')).toContainText('version 1')
    await expect(main).toContainText('Native sources remain read-only.')
    await expect(main).toContainText('Editing, rewrite behavior and Lua output remain outside this workflow.')
    await expect(main.getByText('Management token required')).toHaveCount(0)

    const rendered = await main.innerText()
    expect(rendered).not.toContain('/etc/ssl/certs/site.pem')
    expect(rendered).not.toContain('private-key-secret')
    expect(await main.locator('textarea, [contenteditable="true"], input:not([type="search"])').count()).toBe(0)

    const importRequests = requested.filter(({ path }) => path.startsWith('/api/v1/import-reports'))
    expect(importRequests).toEqual([
      { path: '/api/v1/import-reports', method: 'GET', authorization: `Bearer ${MANAGEMENT_TOKEN}` },
      { path: '/api/v1/import-reports/0', method: 'GET', authorization: `Bearer ${MANAGEMENT_TOKEN}` },
    ])
    expect(requested.some(({ path }) => path === '/api/v1/import')).toBe(false)
    expect(external).toEqual([])
    await expectNoPageScroll(page)
  })

  test('says why a blocked candidate has no preview', async ({ page }) => {
    await mockReports(page, true)
    await page.goto('/#/provenance')
    await unlock(page)

    const main = page.getByRole('main')
    await expect(main).toContainText('Blocked')
    await expect(main.getByRole('table', { name: 'Conversion blockers' })).toContainText('E_REWRITE_UNSUPPORTED')
    await expect(main).toContainText('No preview was produced.')
    await expect(main.locator('pre')).toHaveCount(0)
  })

  test('reaches the preview from the keyboard', async ({ page }) => {
    await mockReports(page)
    await page.goto('/#/provenance')
    await unlock(page)

    const preview = page.getByRole('region', { name: 'KDL preview text' })
    await preview.focus()
    await expect(preview).toBeFocused()
  })

  for (const theme of THEMES) {
    for (const blocked of [false, true]) {
      test(`has no accessibility violations in the ${theme} theme for a ${blocked ? 'blocked' : 'finalized'} report`, async ({ page }) => {
        await useTheme(page, theme)
        await mockReports(page, blocked)
        await page.goto('/#/provenance')
        await unlock(page)
        await expect(page.getByRole('heading', { name: 'Canonical field origins' })).toBeVisible()
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
        await expectNoAxeViolations(page)
        await expectNoPageScroll(page)
      })
    }
  }
})
