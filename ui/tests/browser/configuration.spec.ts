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

test.describe('configuration workspace browser gates', () => {
  test('unlocks configuration, re-locks after authorization failure, and preserves the draft', async ({ page }) => {
    let validationRequests = 0
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/config' && request.method() === 'GET') return json(configSnapshot())
      if (path === '/api/v1/config/validate') {
        validationRequests += 1
        return json({ error: { code: 'unauthorized', message: 'expired browser token' } }, 401)
      }
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') return shutdownStream()
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
      if (path === '/api/v2/events/stream') {
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
})
