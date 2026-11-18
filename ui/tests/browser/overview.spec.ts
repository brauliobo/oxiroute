import { expect, test } from '@playwright/test'

import { dashboardResponse, installApiMock, requestPath, unlock } from './support'
import { demoDashboardResponse } from './support/dashboard'

test.describe('overview workspace browser gates', () => {
  test('renders deterministic desktop and mobile dashboard states without external requests', async ({ page }) => {
    await installApiMock(page, (request) => dashboardResponse(requestPath(request)))

    await page.goto('/#/overview')
    await unlock(page)

    await expect(page.getByRole('heading', { name: 'Monitoring overview' })).toBeVisible()
    await expect(page.locator('[data-section="traffic"]')).toContainText('42')
    await expect(page.locator('[data-section="listeners"]')).toContainText('HTTP ingress')
    await expect(page.locator('[data-section="listeners"]').getByText('FAILED')).toBeVisible()
    await expect(page.locator('[data-section="pools"]')).toContainText('web-backends')
    await expect(page.locator('[data-section="recorders"]').first()).toContainText('Recording supported')

    await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toHaveCSS('display', 'flex')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  })

  test('shows empty RTMP sections as no data yet, never as the epoch', async ({ page }) => {
    await installApiMock(page, (request) => {
      const path = requestPath(request)
      if (path === '/api/v1/rtmp/streams') return { body: { revision: '0', as_of_unix_ms: 0, capabilities: { live_ingest: false, manual_recording: false }, streams: [], recently_ended_streams: [] } }
      if (path === '/api/v1/rtmp/stats') {
        return { body: { revision: '0', asOfUnixMs: 0, global: { activeStreams: 0, publishers: 0, subscribers: 0, audioPayloadBytes: '0', videoPayloadBytes: '0', liveIngest: false, manualRecording: false }, live: [], clients: [], liveTruncated: false, clientsTruncated: false } }
      }
      return demoDashboardResponse(path)
    })

    await page.goto('/#/overview')
    await unlock(page)

    await expect(page.getByText('Snapshot: no data yet')).toHaveCount(2)
    await expect(page.locator('body')).not.toContainText('1970')
    await expect(page.locator('body')).not.toContainText('21:00:00')
  })

  test('writes every time as locale-independent UTC and every byte count in IEC units', async ({ page }) => {
    await installApiMock(page, (request) => demoDashboardResponse(requestPath(request)))

    await page.goto('/#/overview')
    await unlock(page)

    await expect(page.locator('[data-section="monitoring"]')).toContainText('Snapshot 2025-06-15 15:06:40 UTC')
    await expect(page.locator('[data-section="listeners"]')).toContainText('512 KiB')
    await expect(page.locator('body')).not.toContainText(/\b\d+(\.\d+)? (KB|MB|GB)\b/)
  })

  test('keeps long tables reachable on a phone without a horizontal page scroll', async ({ page }) => {
    await installApiMock(page, (request) => demoDashboardResponse(requestPath(request)))

    await page.goto('/#/stats')
    await unlock(page)
    await expect(page.getByRole('heading', { name: 'Upstream pools' })).toBeVisible()

    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
    const scrollable = await page.evaluate(() => [...document.querySelectorAll<HTMLElement>('main *')]
      .filter((element) => element.scrollWidth > element.clientWidth && ['auto', 'scroll'].includes(getComputedStyle(element).overflowX))
      .map((element) => element.tabIndex >= 0 || element.querySelector('a, button, [tabindex]') !== null))
    expect(scrollable.every(Boolean)).toBe(true)
  })
})
