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

test.describe('provenance workspace browser gates', () => {
  test('browses redacted native import reports without crossing the offline read-only boundary', async ({ page }) => {
    const selectedReport = importReportResponse()
    const inventory = structuredClone(selectedReport)
    inventory.selection = null
    inventory.report = null
    inventory.preview = null
    const requestedPaths: Array<{ path: string; method: string; authorization: string | undefined }> = []
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      requestedPaths.push({ path, method: request.method(), authorization: request.headers()['authorization'] })
      if (path === '/api/v1/monitoring') return managementMonitoring()
      if (path === '/api/v1/import-reports' && request.method() === 'GET') return json(inventory)
      if (path === '/api/v1/import-reports/0' && request.method() === 'GET') return json(selectedReport)
      return undefined
    })

    await page.goto('/#/provenance')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    const provenance = page.locator('.provenance-workspace')
    await expect(provenance.locator('#provenance-heading')).toBeVisible()
    await expect(provenance.locator('.report-identity')).toContainText('apache import')
    await expect(provenance.locator('.report-identity')).toContainText('apache-static-reverse-proxy')
    await expect(provenance.locator('.status-chip')).toHaveText('finalized')
    await expect(provenance.locator('.selection-panel')).toContainText('1 report retained')
    await expect(provenance.locator('.source-list')).toContainText('Source 1')
    await expect(provenance.locator('.evidence-panel .evidence-list')).toContainText('Source 1 -> source 2')
    await expect(provenance.locator('.provenance-panel')).toContainText('/http_services/0/routes/0/action')
    await expect(provenance.locator('.requirements-panel')).toContainText('2 retained')
    await expect(provenance.locator('.preview-panel pre')).toContainText('version 1')
    await expect(provenance.locator('.boundary-note')).toContainText('Native sources remain read-only.')
    await expect(provenance.locator('.boundary-note')).toContainText('Editing, rewrite behavior, and Lua output remain outside this workflow.')

    const rendered = await provenance.innerText()
    expect(rendered).not.toContain('/etc/ssl/certs/site.pem')
    expect(rendered).not.toContain('private-key-secret')
    expect(await provenance.locator('input, textarea, [contenteditable="true"]').count()).toBe(0)

    const importRequests = requestedPaths.filter(({ path }) => path.startsWith('/api/v1/import-reports'))
    expect(importRequests).toEqual([
      { path: '/api/v1/import-reports', method: 'GET', authorization: `Bearer ${MANAGEMENT_TOKEN}` },
      { path: '/api/v1/import-reports/0', method: 'GET', authorization: `Bearer ${MANAGEMENT_TOKEN}` },
    ])
    expect(requestedPaths.some(({ path }) => path === '/api/v1/import')).toBe(false)
    expect(importRequests.every(({ method }) => method === 'GET')).toBe(true)

    const viewport = page.viewportSize()
    expect(viewport).not.toBeNull()
    const navigationDisplay = await page.locator('.app-navigation').evaluate((element) => getComputedStyle(element).display)
    const metricColumns = await provenance.locator('.report-metrics').evaluate((element) =>
      getComputedStyle(element).gridTemplateColumns.trim().split(/\s+/).length,
    )
    const reportHeadingDirection = await provenance.locator('.report-heading').evaluate((element) => getComputedStyle(element).flexDirection)
    if ((viewport?.width ?? 0) <= 700) {
      expect(navigationDisplay).toBe('grid')
      expect(metricColumns).toBe((viewport?.width ?? 0) <= 420 ? 1 : 2)
      expect(reportHeadingDirection).toBe('column')
    } else {
      expect(navigationDisplay).toBe('flex')
      expect(metricColumns).toBe(5)
      expect(reportHeadingDirection).toBe('row')
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  })
})
