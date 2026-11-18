import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { installApiMock, json, managementMonitoring, requestPath, shutdownStream, unlock } from './support'
import { THEMES, expectNoAxeViolations, expectNoPageScroll, trackExternalRequests, useTheme } from './support/lists'
import { durableAuditPage, durableAuditStatus } from '../../src/test/managementFixtures'

interface Mocked {
  auditUrls: string[]
  eventStreamRequests: () => number
}

async function mockAudit(page: Page, status = durableAuditStatus(), hasMore = false): Promise<Mocked> {
  const first = durableAuditPage()
  first.hasMore = hasMore
  first.latestCursor = hasMore ? 13 : 12
  const next = durableAuditPage()
  next.records[0]!.id = 13
  next.cursor = 13
  next.latestCursor = 13
  const auditUrls: string[] = []
  let eventStreamRequests = 0
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    if (path === '/api/v1/monitoring') return managementMonitoring()
    if (path === '/api/v1/audit/status') return json(status)
    if (path === '/api/v1/audit') {
      const url = new URL(request.url())
      auditUrls.push(url.toString())
      return json(url.searchParams.get('after') === '12' ? next : first)
    }
    if (path === '/api/v2/events/stream') {
      eventStreamRequests += 1
      return shutdownStream()
    }
    return undefined
  })
  return { auditUrls, eventStreamRequests: () => eventStreamRequests }
}

test.describe('audit workspace browser gates', () => {
  test('browses durable audit records with filters and cursor pagination without SSE fallback', async ({ page }) => {
    const external = trackExternalRequests(page)
    const mocked = await mockAudit(page, durableAuditStatus(), true)

    await page.goto('/#/audit')
    await unlock(page)
    await expect(page.getByRole('heading', { name: 'Audit', exact: true })).toBeVisible()
    const table = page.getByRole('table', { name: 'Redacted durable audit records' })
    await expect(table).toContainText('server_update')
    await expect(page.getByText('Persistent', { exact: true })).toBeVisible()
    await expect(page.getByText('Management token required')).toHaveCount(0)

    await page.getByLabel('Category').selectOption('control')
    await page.getByLabel('Result').selectOption('succeeded')
    await page.getByRole('button', { name: 'Apply filters' }).click()
    await expect.poll(() => mocked.auditUrls.some((url) => url.includes('category=control') && url.includes('result=succeeded'))).toBe(true)
    await page.getByRole('button', { name: 'Load more durable records' }).click()
    await expect(table.locator('tbody tr')).toHaveCount(2)
    await expect(table).toContainText('13')

    await page.getByLabel('Category').focus()
    await expect(page.getByLabel('Category')).toBeFocused()
    expect(mocked.eventStreamRequests()).toBe(0)
    expect(external).toEqual([])
    await expectNoPageScroll(page)
  })

  test('explains a degraded store with its reason and what to check', async ({ page }) => {
    const status = durableAuditStatus()
    status.audit.state = 'degraded'
    status.audit.degraded = true
    status.audit.lastError = 'root_open_failed'
    await mockAudit(page, status)

    await page.goto('/#/audit')
    await unlock(page)

    const banner = page.locator('.ox-banner', { hasText: 'Durable audit is degraded.' })
    await expect(banner).toContainText('root_open_failed')
    await expect(banner).toContainText('real directories the daemon may create and open')
    await expect(page.getByRole('status').filter({ hasText: 'Degraded' }).first()).toBeVisible()
  })

  test('keeps the records table reachable from the keyboard', async ({ page }) => {
    await mockAudit(page)
    await page.goto('/#/audit')
    await unlock(page)

    const region = page.getByRole('region', { name: 'Audit records table' })
    await region.focus()
    await expect(region).toBeFocused()
  })

  for (const theme of THEMES) {
    test(`has no accessibility violations in the ${theme} theme with a degraded store`, async ({ page }) => {
      const degraded = durableAuditStatus()
      degraded.audit.state = 'degraded'
      degraded.audit.degraded = true
      degraded.audit.lastError = 'io_failed'
      await useTheme(page, theme)
      await mockAudit(page, degraded)
      await page.goto('/#/audit')
      await unlock(page)
      await expect(page.locator('tbody tr')).toHaveCount(1)
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
      await expectNoAxeViolations(page)
      await expectNoPageScroll(page)
    })
  }
})
