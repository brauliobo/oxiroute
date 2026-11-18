import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { installApiMock, json, managementMonitoring, requestPath, shutdownStream, sse, unlock } from './support'
import { THEMES, expectNoAxeViolations, expectNoPageScroll, trackExternalRequests, useTheme } from './support/lists'

const at = (cursor: number) => 1_754_000_000_000 + cursor * 60_000

const demoEvents = [
  { cursor: 1, timestampUnixMs: at(1), event: 'generation_prepare', outcome: 'prepared', revision: 'revision-demo-0001' },
  { cursor: 2, timestampUnixMs: at(2), event: 'generation_activate', outcome: 'activated', revision: 'revision-demo-0001' },
  { cursor: 3, timestampUnixMs: at(3), event: 'certificate_renewal', outcome: 'failed', revision: null, certificate: 'edge.example.com' },
  {
    cursor: 4,
    timestampUnixMs: at(4),
    event: 'upstream_endpoint_ejection',
    outcome: {
      type: 'ejected', pool: 'web', server: 'backend-1', reason: 'connect_failed',
      failureCount: 3, ejectionCount: 1, ejectedAtUnixMs: at(4), ejectionUntilUnixMs: at(5),
    },
    revision: null,
  },
]

const eventPage = (events: unknown[], latest: number) => ({
  events, cursor: latest, latestCursor: latest, hasMore: false, oldestCursor: events.length ? 1 : null,
})

async function mockEvents(page: Page, pages: unknown[] = [eventPage(demoEvents, 4)], stream = shutdownStream()): Promise<void> {
  let served = 0
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    if (path === '/api/v1/monitoring') return managementMonitoring()
    if (path === '/api/v2/events') return json(pages[Math.min(served++, pages.length - 1)])
    if (path === '/api/v2/events/stream') return stream
    return undefined
  })
}

test.describe('events workspace browser gates', () => {
  test('lists retained events in a table without external requests or a management token panel', async ({ page }) => {
    const external = trackExternalRequests(page)
    await mockEvents(page)
    await page.goto('/#/events')
    await unlock(page)

    const table = page.getByRole('table', { name: 'Operational events retained in the event ring' })
    await expect(table.locator('tbody tr')).toHaveCount(4)
    await expect(table).toContainText('Generation activate')
    await expect(table).toContainText('backend-1 / connect failed')
    await expect(page.getByText('4 events')).toBeVisible()
    await expect(page.getByText('Management token required')).toHaveCount(0)
    expect(external).toEqual([])
  })

  test('filters by search text and by event name', async ({ page }) => {
    await mockEvents(page)
    await page.goto('/#/events')
    await unlock(page)

    const rows = page.locator('tbody tr')
    await page.getByLabel('Search events').fill('edge.example')
    await expect(rows).toHaveCount(1)
    await expect(page.getByText('Showing 1 of 4 events')).toBeVisible()

    await page.getByLabel('Search events').fill('')
    await page.getByLabel('Event', { exact: true }).selectOption('generation_activate')
    await expect(rows).toHaveCount(1)
    await expect(rows.first()).toContainText('Activated')

    await page.getByLabel('Search events').fill('no such event')
    await expect(page.getByText('No matching events')).toBeVisible()
  })

  test('says Live while connected and marks a restarted ring after a reload', async ({ page }) => {
    await mockEvents(
      page,
      [eventPage(demoEvents, 4), eventPage(demoEvents.slice(0, 1), 1)],
      sse('event: ready\ndata: {"cursor":4}\n\n'),
    )
    await page.goto('/#/events')
    await unlock(page)

    await expect(page.getByRole('status').filter({ hasText: 'Live' })).toBeVisible()
    await expect(page.getByText('The event ring restarted.')).toHaveCount(0)
    await page.getByRole('button', { name: 'Reload history' }).click()
    await expect(page.getByText('The event ring restarted. Earlier history was lost.')).toBeVisible()
    await expect(page.getByText('latest cursor is now 1, it was 4')).toBeVisible()
    await expect(page.locator('tbody tr')).toHaveCount(1)
  })

  test('says Offline after the server closes live updates', async ({ page }) => {
    await mockEvents(page)
    await page.goto('/#/events')
    await unlock(page)

    await expect(page.getByRole('status').filter({ hasText: 'Offline' })).toBeVisible()
    await expect(page.getByText('The server closed live updates because it is shutting down.')).toBeVisible()
  })

  for (const theme of THEMES) {
    test(`has no accessibility violations and no page scroll in the ${theme} theme`, async ({ page }) => {
      await useTheme(page, theme)
      await mockEvents(page)
      await page.goto('/#/events')
      await unlock(page)
      await expect(page.locator('tbody tr')).toHaveCount(4)
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
      await expectNoAxeViolations(page)
      await expectNoPageScroll(page)
    })
  }
})
