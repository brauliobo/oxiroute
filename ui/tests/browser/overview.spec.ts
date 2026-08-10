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

test.describe('overview workspace browser gates', () => {
  test('renders deterministic desktop and mobile dashboard states without external requests', async ({ page }) => {
    await installApiMock(page, (request) => dashboardResponse(requestPath(request)))

    await page.goto('/#/overview')

    await expect(page.getByRole('heading', { name: 'Monitoring overview' })).toBeVisible()
    await expect(page.locator('.traffic-panel')).toContainText('42')
    await expect(page.locator('.listener-list')).toContainText('HTTP ingress')
    await expect(page.locator('.listener-failed .listener-state')).toHaveText('Failed')
    await expect(page.locator('.pool-section')).toContainText('web-backends')
    await expect(page.locator('.recorder-panel')).toContainText('Recording supported')

    const viewport = page.viewportSize()
    expect(viewport).not.toBeNull()
    const navigationDisplay = await page.locator('.app-navigation').evaluate((element) => getComputedStyle(element).display)
    if ((viewport?.width ?? 0) <= 700) expect(navigationDisplay).toBe('grid')
    else expect(navigationDisplay).toBe('flex')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  })
})
