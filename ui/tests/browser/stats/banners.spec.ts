import { expect, test } from '@playwright/test'

import { PAGE, serveStats } from './support'

const UNAVAILABLE = { status: 503, body: { error: { code: 'stats_unavailable', message: 'x' } } }

test.describe('stats page banners', () => {
  test('says statistics are unavailable instead of showing numbers, then recovers on retry', async ({ page }) => {
    const served = await serveStats(page, { failure: UNAVAILABLE })
    await page.goto(PAGE)
    const alert = page.getByRole('alert')
    await expect(alert).toContainText('Statistics are unavailable (the server cannot sample statistics right now).')
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'error')
    await expect(page.getByRole('contentinfo')).not.toContainText('generation')
    served.failWith(undefined)
    await page.getByRole('button', { name: 'Retry now' }).click()
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'live')
    await expect(alert).toHaveText('')
    await expect(page.getByRole('contentinfo')).toContainText('generation 4')
  })

  test('keeps the last values and says it is retrying when a refresh fails', async ({ page }) => {
    const served = await serveStats(page)
    await page.goto(`${PAGE}?interval=1000`)
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'live')
    served.failWith(UNAVAILABLE)
    await expect(page.getByRole('alert')).toContainText('Showing the last values; retrying in')
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'retrying')
    await expect(page.getByRole('contentinfo')).toContainText('generation 4')
  })

  test('asks for a reload when the server speaks a newer format', async ({ page }) => {
    await serveStats(page, { document: { schemaVersion: 2 as 1 } })
    await page.goto(PAGE)
    await expect(page.getByRole('alert')).toContainText('Reload the page')
    await expect(page.getByRole('button', { name: 'Reload page' })).toBeVisible()
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'unsupported')
  })
})
