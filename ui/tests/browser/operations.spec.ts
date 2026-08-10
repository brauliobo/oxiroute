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

test.describe('operations workspace browser gates', () => {
  test('drives revision-checked operational controls', async ({ page }) => {
    const mutations: Array<{ path: string; method: string; body: unknown }> = []
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/monitoring') return managementMonitoring()
      if (path === '/api/v1/status') return json(managementStatus())
      if (path === '/api/v1/generations') return json(managementGeneration())
      if (path === '/api/v1/listeners') return json(managementListeners())
      if (path === '/api/v1/pools') return json(managementPools())
      if (path === '/api/v1/servers') return json(managementServers())
      if (request.method() === 'POST' || request.method() === 'PUT') {
        mutations.push({ path, method: request.method(), body: requestBody(request) })
        return json({ outcome: 'applied', changed: 1 })
      }
      return undefined
    })
    page.on('dialog', (dialog) => void dialog.accept())

    await page.goto('/#/operations')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    await expect(page.getByRole('heading', { name: 'Runtime status' })).toBeVisible()

    const reloadRequest = page.waitForRequest((request) =>
      requestPath(request) === '/api/v1/generations/reload' && request.method() === 'POST',
    )
    await page.locator('[data-generation-action="reload"]').click()
    await reloadRequest
    await expect.poll(() => mutations.length).toBe(1)

    const listeners = page.locator('section[aria-labelledby="listener-inventory-heading"]')
    const listenerDrain = listeners.getByRole('button', { name: 'Drain', exact: true }).first()
    await expect(listenerDrain).toBeEnabled()
    const drainRequest = page.waitForRequest((request) =>
      requestPath(request) === '/api/v1/listeners/administrative-state' && request.method() === 'POST',
    )
    await listenerDrain.click()
    await drainRequest
    await expect.poll(() => mutations.length).toBe(2)
    const servers = page.locator('section[aria-labelledby="server-inventory-heading"]')
    const forceDown = servers.getByRole('button', { name: 'Force down', exact: true }).first()
    await expect(forceDown).toBeEnabled()
    const forceDownRequest = page.waitForRequest((request) =>
      requestPath(request) === '/api/v1/servers/health-override' && request.method() === 'POST',
    )
    await forceDown.click()
    await forceDownRequest
    await expect.poll(() => mutations.length).toBe(3)

    expect(mutations).toEqual(expect.arrayContaining([
      {
        path: '/api/v1/generations/reload',
        method: 'POST',
        body: { expectedActiveRevision: 'active-revision' },
      },
      {
        path: '/api/v1/listeners/administrative-state',
        method: 'POST',
        body: { listeners: ['HTTP ingress'], state: 'drain', expectedActiveRevision: 'active-revision' },
      },
      {
        path: '/api/v1/servers/health-override',
        method: 'POST',
        body: { targets: [{ pool: 'web-backends', server: 'primary' }], health: 'down', expectedActiveRevision: 'active-revision' },
      },
    ]))
  })
})
