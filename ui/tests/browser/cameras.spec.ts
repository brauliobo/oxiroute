import { expect, test } from '@playwright/test'
import type { Locator, Page } from '@playwright/test'

import { json, unlock } from './support'
import { TICKET, mockCameras } from './support/cameras'
import type { CameraOverride } from './support/cameras'

async function openStreams(page: Page, override?: CameraOverride) {
  const mock = await mockCameras(page, { override })
  await page.goto('/#/cameras')
  await unlock(page)
  await expect(page.locator('[data-camera]')).toHaveCount(6)
  return mock
}

const tile = (page: Page, name: string): Locator => page.locator(`[data-camera="${name}"]`)
const noPageScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)

test.describe('streams grid', () => {
  test('shows every stream with its state word, problems first, and fits the viewport', async ({ page }) => {
    await openStreams(page)

    const states = await page.locator('[data-camera]').evaluateAll((tiles) =>
      tiles.map((element) => [element.getAttribute('data-camera'), element.querySelector('.ox-badge')?.textContent?.trim()]))
    expect(states).toEqual([
      ['cam-garage', 'DEGRADED'], ['cam-side', 'DEGRADED'], ['cam-front', 'LIVE'],
      ['cam-spare', 'IDLE'], ['cam-yard', 'OFFLINE'], ['cam-lobby', 'OFFLINE'],
    ])
    expect(await noPageScroll(page)).toBe(true)
    await expect(page).toHaveTitle('Cameras and recordings - OxiRoute')
  })

  test('shows picture, codecs, bitrate, publisher, viewers and uptime of a live camera', async ({ page }) => {
    await openStreams(page)
    const front = tile(page, 'cam-front')
    for (const text of ['1920x1080 at 30 fps', 'AVC / AAC', '4.1 Mbit/s', '192.0.2.10', '3 viewers', 'RECORDING']) {
      await expect(front).toContainText(text)
    }
    await expect(front.getByText(/^1h 0m$/)).toBeVisible()
  })

  test('shows offline cameras from the catalog and degraded ones with their reasons', async ({ page }) => {
    await openStreams(page)
    await expect(tile(page, 'cam-yard')).toContainText('The publisher stopped sending media.')
    await expect(tile(page, 'cam-yard')).toContainText('1280x720')
    await expect(tile(page, 'cam-yard').getByRole('button')).toHaveCount(0)
    await expect(tile(page, 'cam-lobby')).toContainText('An operator dropped the publisher.')
    await expect(tile(page, 'cam-garage')).toContainText('quota_files')
    await expect(tile(page, 'cam-side').getByRole('list', { name: 'Problems' })).toContainText('No media for')
  })

  test('filters by state and by search, and says when nothing matches', async ({ page }) => {
    await openStreams(page)
    await page.getByRole('button', { name: 'Offline 2' }).click()
    await expect(page.locator('[data-camera]')).toHaveCount(2)
    await expect(page.getByRole('button', { name: 'Offline 2' })).toHaveAttribute('aria-pressed', 'true')
    await page.getByRole('button', { name: 'All 6' }).click()
    await page.getByLabel('Find a stream').fill('garage')
    await expect(page.locator('[data-camera]')).toHaveCount(1)
    await page.getByLabel('Find a stream').fill('nothing-like-this')
    await expect(page.getByText('No stream matches')).toBeVisible()
    await page.getByRole('button', { name: 'Clear filters' }).click()
    await expect(page.locator('[data-camera]')).toHaveCount(6)
  })

  test('shows an unreachable server without dropping the streams already shown', async ({ page }) => {
    let failing = false
    await openStreams(page, (request) => failing && new URL(request.url()).pathname === '/api/v1/rtmp/streams'
      ? json({ error: { code: 'unavailable', message: 'The RTMP catalog is not available.' } }, 503)
      : undefined)
    failing = true
    await page.getByRole('button', { name: 'Refresh now' }).click()
    await expect(page.getByRole('alert').filter({ hasText: 'Stream refresh failed.' })).toContainText('The last valid streams stay visible.')
    await expect(page.locator('[data-camera]')).toHaveCount(6)
  })
})

test.describe('live preview', () => {
  test('plays the ticketed playlist with the keyboard and never exposes the ticket', async ({ page, browserName }) => {
    test.skip(browserName !== 'chromium', 'native HLS is only guaranteed in Chromium here')
    const violations: string[] = []
    await page.exposeFunction('reportViolation', (directive: string) => violations.push(directive))
    await page.addInitScript(() => document.addEventListener('securitypolicyviolation', (event) => {
      void (window as unknown as { reportViolation(directive: string): void }).reportViolation(event.violatedDirective)
    }))
    const mock = await openStreams(page)

    const preview = tile(page, 'cam-front').getByRole('button', { name: 'Preview live / cam-front' })
    await preview.focus()
    await page.keyboard.press('Enter')

    const video = tile(page, 'cam-front').locator('video')
    await expect(video).toHaveAttribute('aria-label', 'Live preview of live / cam-front')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => [element.readyState >= 2, element.videoWidth])).toEqual([true, 320])

    await expect(page.getByRole('button', { name: 'Stop preview of live / cam-front' })).toHaveAttribute('aria-pressed', 'true')
    await expect(page.locator('body')).not.toContainText(TICKET)
    const media = mock.sent.filter(({ path }) => path.endsWith('.m3u8') || path.endsWith('.ts'))
    expect(media.length).toBeGreaterThanOrEqual(2)
    expect(media.every(({ authorization, query }) => authorization === null && query === `?ticket=${TICKET}`)).toBe(true)
    const minted = mock.mutations().filter(({ path }) => path.endsWith('/ticket'))
    expect(minted).toEqual([expect.objectContaining({
      path: '/api/v1/rtmp/media/edge/live/cam-front/ticket', authorization: 'Bearer browser-test-management-token', body: { ttlSeconds: 600 },
    })])
    expect(violations).toEqual([])

    await page.keyboard.press('Enter')
    await expect(video).toHaveCount(0)
  })

  test('explains an application without a media output', async ({ page }) => {
    await openStreams(page, (request) => request.url().endsWith('/ticket')
      ? json({ error: { code: 'media_not_found', message: 'the application has no HLS or DASH output' } }, 404)
      : undefined)
    await tile(page, 'cam-front').getByRole('button', { name: 'Preview live / cam-front' }).click()
    await expect(tile(page, 'cam-front')).toContainText('This application has no HLS output')
    await expect(tile(page, 'cam-front').locator('video')).toHaveCount(0)
  })
})

test.describe('management actions', () => {
  test('starts a manual recorder and keeps the result until it is dismissed', async ({ page }) => {
    const mock = await openStreams(page)
    await tile(page, 'cam-front').getByRole('button', { name: 'Start recording live / cam-front with archive' }).click()

    const result = page.locator('[data-feedback] [role="status"]')
    await expect(result).toContainText('Recording live / cam-front with archive was requested.')
    expect(mock.mutations().map(({ path }) => path)).toEqual(['/api/v1/rtmp/streams/stream-front/recorders/rec-archive/start'])
    await page.getByRole('button', { name: 'Refresh now' }).click()
    await expect(result).toContainText('Recording live / cam-front with archive was requested.')
    await result.getByRole('button', { name: 'Dismiss' }).click()
    await expect(result).toHaveText('')
  })

  test('drops a publisher only after a dialog that names the stream, the address and the audience', async ({ page }) => {
    const mock = await openStreams(page)
    await tile(page, 'cam-front').getByRole('button', { name: 'Drop publisher of live / cam-front' }).click()

    const dialog = page.getByRole('dialog', { name: 'Drop the publisher of live / cam-front' })
    await expect(dialog).toContainText('192.0.2.10')
    await expect(dialog).toContainText('3 viewers will lose the stream')
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    expect(mock.mutations()).toEqual([])

    await tile(page, 'cam-front').getByRole('button', { name: 'Drop publisher of live / cam-front' }).click()
    await dialog.getByRole('button', { name: 'Drop publisher' }).click()
    await expect(page.locator('[data-feedback] [role="status"]')).toContainText('The publisher of live / cam-front was asked to disconnect.')
    expect(mock.mutations()).toEqual([expect.objectContaining({ path: '/api/v1/rtmp/clients/sess-front-pub/publisher/drop' })])
  })

  test('lists viewers and drops one by name', async ({ page }) => {
    const mock = await openStreams(page)
    await tile(page, 'cam-front').getByRole('button', { name: '3 viewers' }).click()
    await expect(tile(page, 'cam-front').getByRole('list', { name: 'Viewers of live / cam-front' })).toContainText('198.51.100.21')
    await tile(page, 'cam-front').getByRole('button', { name: 'Drop viewer 198.51.100.21 of live / cam-front' }).click()
    await page.getByRole('dialog', { name: 'Drop the viewer 198.51.100.21' }).getByRole('button', { name: 'Drop viewer' }).click()
    await expect(page.locator('[data-feedback] [role="status"]')).toContainText('The viewer 198.51.100.21 of live / cam-front was asked to disconnect.')
    expect(mock.mutations()).toEqual([expect.objectContaining({ path: '/api/v1/rtmp/clients/sess-view-1/subscriber/drop' })])
  })

  test('reports a failed command in words', async ({ page }) => {
    await openStreams(page, (request) => request.url().includes('/recorders/')
      ? json({ error: { code: 'rtmp_state_conflict', message: 'The recorder is already starting.' } }, 409)
      : undefined)
    await tile(page, 'cam-front').getByRole('button', { name: 'Start recording live / cam-front with archive' }).click()
    await expect(page.locator('[data-feedback] [role="alert"]')).toContainText('Could not start recording live / cam-front: The recorder is already starting.')
  })
})
