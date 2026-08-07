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

  test('preserves redacted RTMP token secrets through an unrelated validate and save', async ({ page }) => {
    const snapshot = redactedRtmpConfigSnapshot()
    let validated: CanonicalConfig | undefined
    let saved: CanonicalConfig | undefined
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(snapshot)
      if (path === '/api/v1/config/validate') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        validated = body.config
        return json(configValidation(body.config))
      }
      if (path === '/api/v1/config' && request.method() === 'PUT') {
        saved = requestBody<{ config: CanonicalConfig }>(request).config
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
    await page.getByRole('button', { name: 'Save canonical configuration' }).click()

    await expect(page.locator('.revision-banner.save-state')).toContainText('Configuration saved; activation pending.')
    expect(validated?.version).toBe(2)
    expect(validated?.rtmp_services[0]?.applications[0]?.publish.token?.secret).toBe('<redacted>')
    expect(validated?.rtmp_services[0]?.applications[0]?.play.token?.secret).toBe('<redacted>')
    expect(saved?.version).toBe(2)
    expect(saved?.rtmp_services[0]?.applications[0]?.publish.token?.secret).toBe('<redacted>')
    expect(saved?.rtmp_services[0]?.applications[0]?.play.token?.secret).toBe('<redacted>')
  })

  test('selects TLS-ALPN-01 by keyboard on mobile without retaining DNS provider fields', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'mobile-chromium', 'This gate exercises the mobile selector.')
    const validated: CanonicalConfig[] = []
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(managedAcmeConfigSnapshot())
      if (path === '/api/v1/config/validate') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        validated.push(body.config)
        return json(configValidation(body.config))
      }
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    await page.locator('#mobile-object-navigation').selectOption('certificates:0')

    const challenge = page.locator('[data-field="certificates[].source.challenge"] select')
    await expect(challenge).toHaveValue('http01')
    await expect(challenge.locator('option')).toHaveCount(3)
    await challenge.focus()
    await challenge.press('ArrowDown')
    await expect(challenge).toHaveValue('dns01')
    await page.locator('[data-field="certificates[].source.dns01.provider"] input').fill('route53')
    await page.locator('[data-field="certificates[].source.dns01.credential_file"] input').fill('/run/dns-token')
    await challenge.press('ArrowDown')
    await expect(challenge).toHaveValue('tls_alpn01')
    await expect(page.locator('.challenge-note')).toContainText('public TCP port 443')
    await expect(page.locator('.challenge-note')).toContainText('does not create or deploy')
    await expect(page.locator('[data-field="certificates[].source.dns01.provider"]')).toHaveCount(0)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)

    await page.getByRole('button', { name: 'Validate candidate' }).click()
    await expect(page.getByText('KDL configuration preview')).toBeVisible()
    expect(validated).toHaveLength(1)
    const submitted = validated[0]!
    expect(submitted.certificates[0]?.source).toEqual(expect.objectContaining({
      challenge: 'tls_alpn01',
      dns01: null,
    }))
    expect(JSON.stringify(submitted)).not.toContain('route53')
    expect(JSON.stringify(submitted)).not.toContain('/run/dns-token')
  })

  test('prepares a reviewable TLS-ALPN listener draft from the certificate workflow', async ({ page }, testInfo) => {
    let validated: CanonicalConfig | undefined
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(managedAcmeConfigSnapshot())
      if (path === '/api/v1/config/validate') {
        const body = requestBody<{ config: CanonicalConfig }>(request)
        validated = body.config
        return json(configValidation(body.config))
      }
      if (path === '/api/v1/events/stream') return shutdownStream()
      return undefined
    })

    await page.goto('/#/configuration')
    await page.locator('#config-access-token').fill(CONFIG_TOKEN)
    await page.getByRole('button', { name: 'Unlock configuration' }).click()
    if (testInfo.project.name === 'mobile-chromium') {
      await page.locator('#mobile-object-navigation').selectOption('certificates:0')
    } else {
      await page.locator('.object-navigation .object-link').filter({ hasText: 'managed-edge' }).click()
    }
    await page.locator('[data-field="certificates[].source.challenge"] select').selectOption('tls_alpn01')
    await page.getByRole('button', { name: 'Prepare TLS-ALPN listener' }).click()

    await expect(page.locator('.revision-banner.tls-alpn')).toContainText('draft prepared')
    await expect(page.locator('[data-field="listeners[].bind.address"] input')).toHaveValue('0.0.0.0:443')
    await page.getByRole('button', { name: 'Validate candidate' }).click()
    await expect(page.getByText('KDL configuration preview')).toBeVisible()

    expect(validated?.listeners[0]).toMatchObject({
      protocol: 'http',
      bind: { type: 'socket', address: '0.0.0.0:443' },
      tls_profile: 'managed-edge-tls-alpn-profile',
      service: 'managed-edge-tls-alpn-service',
    })
    expect(validated?.tls_profiles[0]?.certificates).toEqual(['managed-edge'])
    expect(validated?.http_services[0]?.routes[0]?.action).toEqual({
      type: 'fixed_response',
      status: 404,
      body: '',
      headers: [],
    })
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
      if (path === '/api/v1/events') return json(managementEventPage())
      if (path === '/api/v1/events/stream') return shutdownStream()
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
      if (path === '/api/v1/events/stream') {
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

function redactedRtmpConfigSnapshot() {
  const snapshot = configSnapshot()
  const service: RtmpServiceConfig = {
    name: 'live',
    outbound_chunk_size: 4_096,
    access_log: null,
    outbound_policy: defaultRtmpOutboundPolicy(),
    callbacks: defaultRtmpCallback(),
    auto_push: defaultRtmpAutoPush(),
    applications: [{
      name: 'broadcast',
      live: true,
      idle_streams: true,
      publish: {
        rules: [],
        token: { source: 'stream_query', parameter: 'token', secret: '<redacted>' },
      },
      play: {
        rules: [],
        token: { source: 'stream_query', parameter: 'viewer', secret: '<redacted>' },
      },
      limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
      push_targets: [],
      pull_targets: [],
      relay: defaultRtmpRelay(),
      callbacks: defaultRtmpCallback(),
      fanout: {
        max_subscribers: 1_024,
        max_queue_messages_per_subscriber: 256,
        max_queue_bytes_per_subscriber: 8_388_608,
      },
      vod: null,
      recorders: [],
    }],
  }
  snapshot.config.rtmp_services = [service]
  return snapshot
}
