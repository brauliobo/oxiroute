import { expect, test } from '@playwright/test'

import {
  CONFIG_TOKEN,
  MANAGEMENT_TOKEN,
  configFixtureWithDiagnostic,
  configSaveResponse,
  configSnapshot,
  configValidation,
  dashboardResponse,
  installApiMock,
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
} from '../../src/test/managementFixtures'
import type { CanonicalConfig } from '../../src/config'

test.describe('dashboard browser gates', () => {
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

  test('unlocks configuration, re-locks after authorization failure, and preserves the draft', async ({ page }) => {
    let validationRequests = 0
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(configSnapshot())
      if (path === '/api/v1/config/validate') {
        validationRequests += 1
        return json({ error: { code: 'unauthorized', message: 'expired browser token' } }, 401)
      }
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await expect(page.locator('.revision-board')).toBeVisible()

    await page.locator('[data-field="version"] input').fill('2')
    await page.getByRole('button', { name: 'Validate candidate' }).click()
    await expect.poll(() => validationRequests).toBe(1)
    await expect(page.locator('form[data-unlock-form]')).toBeVisible()
    await expect(page.locator('.unlock-error')).toContainText('Authorization expired')
    expect(await page.locator('body').textContent()).not.toContain(CONFIG_TOKEN)

    await page.locator('#config-access-token').fill('browser-test-refreshed-token')
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await expect(page.locator('.revision-board')).toContainText('Unsaved changes')
    await expect(page.locator('[data-field="version"] input')).toHaveValue('2')
  })

  test('validates, reviews, and saves a configuration with the disk revision precondition', async ({ page }) => {
    const saves: Array<{ revision: string | undefined; config: CanonicalConfig }> = []
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(configSnapshot())
      if (path === '/api/v1/config/validate') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        return json(configValidation(body.config))
      }
      if (path === '/api/v1/config' && request.method() === 'PUT') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        saves.push({ revision: request.headers()['if-config-revision'], config: body.config })
        return json(configSaveResponse())
      }
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await expect(page.locator('.revision-board')).toBeVisible()
    await page.locator('[data-field="version"] input').fill('2')
    await page.getByRole('button', { name: 'Validate candidate' }).click()
    await expect(page.getByText('KDL configuration preview')).toBeVisible()
    await page.getByRole('button', { name: 'Review save' }).click()
    await expect(page.getByRole('dialog', { name: 'Save review' })).toBeVisible()
    await page.getByRole('button', { name: 'Save canonical configuration' }).click()

    await expect(page.locator('.revision-banner.save-state')).toContainText('Configuration saved; activation pending.')
    expect(saves).toHaveLength(1)
    expect(saves[0]).toMatchObject({ revision: 'disk-revision', config: { version: 2 } })
  })

  test('preserves a dirty draft on save conflict', async ({ page }) => {
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(configSnapshot())
      if (path === '/api/v1/config/validate') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        return json(configValidation(body.config))
      }
      if (path === '/api/v1/config' && request.method() === 'PUT') {
        return json({
          error: { code: 'config_revision_conflict', message: 'the disk revision changed before the write' },
          diskRevision: 'newer-disk-revision',
          activeRevision: 'active-revision',
          diagnostics: [],
        }, 409)
      }
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await page.locator('[data-field="version"] input').fill('2')
    await page.getByRole('button', { name: 'Validate candidate' }).click()
    await page.getByRole('button', { name: 'Review save' }).click()
    await page.getByRole('button', { name: 'Save canonical configuration' }).click()

    await expect(page.locator('.revision-banner.stale')).toContainText('Draft preserved: the disk revision changed.')
    await expect(page.locator('.revision-banner.stale')).toContainText('newer-disk')
    await expect(page.locator('.revision-banner.save-state')).toContainText('Save conflict; draft preserved.')
    await expect(page.locator('[data-field="version"] input')).toHaveValue('2')
  })

  test('shows external edits, reconnects SSE from the last cursor, and never uses production network', async ({ page }) => {
    let configReads = 0
    let streamRequests = 0
    const streamHeaders: Array<Record<string, string>> = []
    let releaseExternalEvent!: () => void
    const externalEvent = new Promise<void>((resolve) => {
      releaseExternalEvent = resolve
    })
    const initial = configSnapshot('disk-revision', 1)
    const updated = configSnapshot('external-disk-revision', 2)

    await installApiMock(page, async (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') {
        configReads += 1
        return json(configReads === 1 ? initial : updated)
      }
      if (path === '/api/v1/events/stream') {
        streamRequests += 1
        streamHeaders.push(request.headers())
        if (streamRequests === 1) {
          await externalEvent
          return sse([
            'event: ready',
            'data: {"cursor":0}',
            '',
            'id: 1',
            'event: generation_activate',
            'data: {"cursor":1,"timestampUnixMs":null,"event":"generation_activate","outcome":"activated","revision":"external-disk-revision"}',
            '',
            '',
          ].join('\n'))
        }
        return shutdownStream()
      }
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await expect(page.locator('.revision-board')).toBeVisible()
    await expect.poll(() => streamRequests).toBe(1)

    await page.locator('[data-field="version"] input').fill('3')
    releaseExternalEvent()
    await expect(page.locator('.revision-banner.stale')).toContainText('Draft preserved: the disk revision changed.')
    await expect(page.locator('.revision-banner.stale')).toContainText('external-dis')
    await expect.poll(() => streamRequests).toBe(2)
    expect(streamHeaders[1]?.['last-event-id']).toBe('1')
    expect(configReads).toBe(2)
  })

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

  test('renders certificate status without leaking private or ACME account material', async ({ page }) => {
    const inventory = managementTlsInventory()
    const managed = inventory.certificates.find((certificate) => certificate.name === 'managed-edge')
    if (managed?.status && 'directoryUrl' in managed.status) {
      Object.assign(managed.status, {
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
      if (path === '/api/v1/events') return json(managementEventPage())
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/certificates')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    await expect(page.getByRole('heading', { name: 'Configured certificates' })).toBeVisible()
    await expect(page.locator('.certificate-grid')).toContainText('managed-edge')
    const text = await page.locator('body').innerText()
    expect(text).not.toContain('PRIVATE-KEY-MATERIAL')
    expect(text).not.toContain('account/secret')
    expect(text).not.toContain('order/secret')
    expect(text).not.toContain('challenge-secret')
  })

  test('displays import source diagnostics and the offline report boundary', async ({ page }) => {
    const requestedPaths: string[] = []
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      requestedPaths.push(path)
      if (path === '/api/v1/monitoring') return managementMonitoring()
      if (path === '/api/v1/config') return json(configFixtureWithDiagnostic())
      return undefined
    })

    await page.goto('/#/provenance')
    await page.locator('#management-access-token').fill(MANAGEMENT_TOKEN)
    await page.getByRole('button', { name: 'Unlock telemetry' }).click()
    const provenance = page.locator('.provenance-workspace')
    await expect(provenance.getByRole('heading', { name: 'Canonical source' })).toBeVisible()
    await expect(provenance.locator('.diagnostic-list')).toContainText('W_IMPORT_PROVENANCE')
    await expect(provenance).toContainText('Native import report unavailable over management API.')
    expect(requestedPaths).not.toContain('/api/v1/import')
  })
})
