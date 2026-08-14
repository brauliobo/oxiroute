import { expect, test } from '@playwright/test'

import {
  CONFIG_TOKEN,
  MANAGEMENT_TOKEN,
  configSaveResponse,
  configSnapshot,
  configValidation,
  dashboardResponse,
  installApiMock,
  managedAcmeConfigSnapshot,
  json,
  managementEventPage,
  managementMonitoring,
  requestBody,
  requestPath,
  sse,
  shutdownStream,
} from './support'
import {
  managementGeneration,
  managementListeners,
  managementPools,
  managementServers,
  managementStatus,
  managementTlsInventory,
  durableAuditPage,
  durableAuditStatus,
} from '../../src/test/managementFixtures'
import { importReportResponse } from '../../src/test/importFixtures'
import { defaultRtmpAutoPush, defaultRtmpCallback, defaultRtmpOutboundPolicy, defaultRtmpRelay } from '../../src/configuration/canonicalDefaults'
import type { CanonicalConfig, RtmpServiceConfig } from '../../src/config'

test.describe('audit workspace browser gates', () => {
  test('browses durable audit records with filters and cursor pagination without SSE fallback', async ({ page }) => {
    const first = durableAuditPage()
    first.hasMore = true
    first.latestCursor = 13
    const next = durableAuditPage()
    next.records[0]!.id = 13
    next.cursor = 13
    next.latestCursor = 13
    const auditUrls: string[] = []
    let eventStreamRequests = 0
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/monitoring') return managementMonitoring()
      if (path === '/api/v1/audit/status') return json(durableAuditStatus())
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

    await page.goto('/#/audit')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    await expect(page.getByRole('heading', { name: 'Audit' })).toBeVisible()
    await expect(page.locator('.audit-record')).toContainText('server_update')
    await expect(page.locator('.status-panel')).toContainText('Persistent')

    await page.locator('#audit-category').selectOption('control')
    await page.locator('#audit-result').selectOption('succeeded')
    await page.getByRole('button', { name: 'Apply filters' }).click()
    await expect.poll(() => auditUrls.some((url) => url.includes('category=control') && url.includes('result=succeeded'))).toBe(true)
    await page.getByRole('button', { name: 'Load more durable records' }).click()
    await expect(page.locator('.audit-list')).toContainText('Record 13')

    await page.locator('#audit-category').focus()
    await expect(page.locator('#audit-category')).toBeFocused()
    expect(eventStreamRequests).toBe(0)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  })
})
