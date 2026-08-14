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

test.describe('certificates workspace browser gates', () => {
  test('renders certificate status without leaking private or ACME account material', async ({ page }) => {
    const inventory = managementTlsInventory()
    const managed = inventory.certificates.find((certificate) => certificate.name === 'managed-edge')
    if (managed?.status && 'directoryUrl' in managed.status) {
      Object.assign(managed.status, {
        challenge: 'tls_alpn01',
        dnsProvider: null,
        privateKey: 'PRIVATE-KEY-MATERIAL',
        accountUrl: 'https://acme.example.test/account/secret',
        orderUrl: 'https://acme.example.test/order/secret',
        authorizationToken: 'challenge-secret',
      })
    }
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/monitoring') return managementMonitoring()
      if (path === '/api/v1/tls') return json(inventory)
      if (path === '/api/v1/generations') return json(managementGeneration())
      if (path === '/api/v2/events') return json(managementEventPage())
      if (path === '/api/v2/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/certificates')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    await expect(page.getByRole('heading', { name: 'Configured certificates' })).toBeVisible()
    await expect(page.locator('.certificate-grid')).toContainText('managed-edge')
    await expect(page.locator('.certificate-grid')).toContainText('tls_alpn01')
    const text = await page.locator('body').innerText()
    expect(text).not.toContain('PRIVATE-KEY-MATERIAL')
    expect(text).not.toContain('account/secret')
    expect(text).not.toContain('order/secret')
    expect(text).not.toContain('challenge-secret')
  })
})
