import { expect, test } from '@playwright/test'

import { wireDocument } from '../../../src/stats/fixtures'
import { PAGE, serveStats } from './support'

const confirmDrain = async (page: import('@playwright/test').Page) => {
  await page.getByRole('button', { name: 'Drain', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Confirm administration' })
  await expect(dialog).toContainText('Set server origin-a of pool web to Drain?')
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
  return dialog
}

test.describe('stats page administration', () => {
  test('is absent unless the server says admin', async ({ page }) => {
    await serveStats(page, { document: { admin: false } })
    await page.goto(PAGE)
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'live')
    await expect(page.getByRole('heading', { name: 'Administration' })).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Drain', exact: true })).toHaveCount(0)
  })

  test('fits the screen with its controls, whatever the width', async ({ page }) => {
    await serveStats(page, { document: { admin: true } })
    await page.goto(PAGE)
    const admin = page.locator('section.ox-admin')
    await expect(admin).toBeVisible()
    const frame = (await admin.boundingBox())!
    for (const control of await admin.locator('.ox-admin-controls select, .ox-admin-controls button').all()) {
      const box = (await control.boundingBox())!
      expect(box.height, 'a control keeps its own height').toBeLessThan(64)
      expect(box.x + box.width, 'a control stays inside the panel').toBeLessThanOrEqual(frame.x + frame.width)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0)
  })

  test('posts the urlencoded form with the active revision after a confirmation', async ({ page }) => {
    const served = await serveStats(page, { document: { admin: true } })
    await page.goto(PAGE)
    await expect(page.getByRole('heading', { name: 'Administration' })).toBeVisible()
    const dialog = await confirmDrain(page)
    expect(served.posts).toEqual([])
    const before = served.snapshots()
    await dialog.getByRole('button', { name: 'Confirm' }).click()
    await expect(page.getByText('Server origin-a of pool web is now draining.')).toBeVisible()
    expect(served.posts).toEqual([
      { generation_revision: wireDocument().generation.activeRevision, pool: 'web', server: 'origin-a', state: 'drain' },
    ])
    await expect.poll(() => served.snapshots()).toBeGreaterThan(before)
  })

  test('cancelling posts nothing and gives focus back to the button', async ({ page }) => {
    const served = await serveStats(page, { document: { admin: true } })
    await page.goto(PAGE)
    const dialog = await confirmDrain(page)
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: 'Drain', exact: true })).toBeFocused()
    expect(served.posts).toEqual([])
  })

  test('shows a refusal as an alert', async ({ page }) => {
    await serveStats(page, {
      document: { admin: true },
      admin: { status: 409, body: { error: { code: 'generation_revision_conflict', message: 'changed' } } },
    })
    await page.goto(PAGE)
    const dialog = await confirmDrain(page)
    await dialog.getByRole('button', { name: 'Confirm' }).click()
    await expect(page.getByRole('alert').filter({ hasText: 'changed since this page loaded' })).toBeVisible()
  })

  test('picks another pool and server', async ({ page }) => {
    const served = await serveStats(page, { document: { admin: true } })
    await page.goto(PAGE)
    await page.getByLabel('Pool', { exact: true }).selectOption('api')
    await page.getByRole('button', { name: 'Maintenance' }).click()
    await page.getByRole('dialog', { name: 'Confirm administration' }).getByRole('button', { name: 'Confirm' }).click()
    await expect.poll(() => served.posts).toMatchObject([{ pool: 'api', server: 'origin-c', state: 'maintenance' }])
  })
})
