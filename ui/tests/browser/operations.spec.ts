import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { managementStatus } from '../../src/test/managementFixtures'
import { json, unlock } from './support'
import { mockOperations } from './support/operations'

const expectedRevision = { expectedActiveRevision: 'active-revision' }
const primary = { targets: [{ pool: 'web-backends', server: 'primary' }] }

async function openOperations(page: Page, tab?: string): Promise<void> {
  await page.goto('/#/operations')
  await unlock(page)
  await expect(page.getByRole('tab', { name: 'Runtime' })).toBeVisible()
  if (tab) await page.getByRole('tab', { name: tab }).click()
}

const drainPrimary = (page: Page) => page.getByRole('button', { name: 'Drain server primary in pool web-backends' })

test.describe('operations workspace', () => {
  test('sends revision-checked controls after the dialog is confirmed', async ({ page }) => {
    const { mutations } = await mockOperations(page)
    await openOperations(page)

    await page.locator('[data-generation-action="reload"]').click()
    await page.getByRole('dialog', { name: 'Reload the persisted configuration' }).getByRole('button', { name: 'Reload' }).click()
    await expect.poll(() => mutations.length).toBe(1)

    await page.getByRole('tab', { name: 'Listeners and pools' }).click()
    await page.getByRole('button', { name: 'Drain listener HTTP ingress' }).click()
    await page.getByRole('dialog', { name: 'Drain listener HTTP ingress' }).getByRole('button', { name: 'Drain', exact: true }).click()
    await expect.poll(() => mutations.length).toBe(2)

    await page.getByRole('tab', { name: 'Servers' }).click()
    await page.getByRole('button', { name: 'Force down server primary in pool web-backends' }).click()
    await page.getByRole('dialog', { name: 'Force server primary down' }).getByRole('button', { name: 'Force down' }).click()
    await expect.poll(() => mutations.length).toBe(3)

    expect(mutations).toEqual([
      { path: '/api/v1/generations/reload', method: 'POST', body: expectedRevision },
      {
        path: '/api/v1/listeners/administrative-state',
        method: 'POST',
        body: { listeners: ['HTTP ingress'], state: 'drain', ...expectedRevision },
      },
      { path: '/api/v1/servers/health-override', method: 'POST', body: { ...primary, health: 'down', ...expectedRevision } },
    ])
  })

  test('keeps the outcome visible and announced after the reload that follows an action', async ({ page }) => {
    const { reads } = await mockOperations(page)
    await openOperations(page, 'Servers')
    const loadsBefore = reads.filter((path) => path === '/api/v1/status').length

    await drainPrimary(page).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Drain', exact: true }).click()

    const outcome = page.getByRole('status').filter({ hasText: 'Server primary in pool web-backends is now draining.' })
    await expect(outcome.first()).toBeVisible()
    await expect(outcome.first()).toContainText('Runtime state only')
    await expect.poll(() => reads.filter((path) => path === '/api/v1/status').length).toBeGreaterThan(loadsBefore)
    await expect(outcome.first()).toBeVisible()
    await expect(page.getByRole('region', { name: 'Notifications' })).toContainText('is now draining')
  })

  test('says what failed for which target in an alert that stays', async ({ page }) => {
    await mockOperations(page, (request) =>
      request.method() === 'POST' && request.url().endsWith('/servers/health-override')
        ? json({ error: { code: 'generation_conflict', message: 'the active generation revision changed' } }, 409)
        : undefined,
    )
    await openOperations(page, 'Servers')

    await page.getByRole('button', { name: 'Force down server primary in pool web-backends' }).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Force down' }).click()

    const alert = page.getByRole('alert').filter({ hasText: 'Could not set the health of server primary in pool web-backends' })
    await expect(alert.first()).toContainText('the active generation revision changed')
    await expect(alert.first()).toBeVisible()
  })

  test('opens a dialog that names the target and lets Escape and Cancel send nothing', async ({ page }) => {
    const { mutations } = await mockOperations(page)
    await openOperations(page, 'Servers')
    const opener = drainPrimary(page)
    const dialog = page.getByRole('dialog', { name: 'Drain server primary' })

    await opener.click()
    await expect(dialog).toBeVisible()
    await expect(dialog).toContainText('Pool: web-backends')
    await expect(dialog).toContainText('Active connections now: 3')
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(opener).toBeFocused()

    await opener.click()
    await dialog.getByRole('button', { name: 'Cancel' }).click()
    await expect(dialog).toBeHidden()
    await expect(opener).toBeFocused()
    expect(mutations).toEqual([])
  })

  test('sends the exact request on Confirm and gives the focus back to the opener', async ({ page }) => {
    const { mutations } = await mockOperations(page)
    await openOperations(page, 'Servers')
    const opener = drainPrimary(page)

    await opener.click()
    await page.getByRole('dialog').getByRole('button', { name: 'Drain', exact: true }).click()

    await expect.poll(() => mutations.length).toBe(1)
    expect(mutations[0]).toEqual({
      path: '/api/v1/servers/administrative-state',
      method: 'POST',
      body: { ...primary, state: 'drain', ...expectedRevision },
    })
    await expect(opener).toBeFocused()
  })

  test('moves the keyboard focus through the dialog and keeps it inside', async ({ page }) => {
    await mockOperations(page)
    await openOperations(page, 'Servers')
    await drainPrimary(page).click()
    const dialog = page.getByRole('dialog', { name: 'Drain server primary' })

    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await page.keyboard.press('Tab')
    await expect(dialog.getByRole('button', { name: 'Drain', exact: true })).toBeFocused()
    await page.keyboard.press('Shift+Tab')
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    for (let step = 0; step < 5; step += 1) {
      await page.keyboard.press('Tab')
      const outside = await page.evaluate(() => {
        const active = document.activeElement
        return active !== document.body && !active?.closest('dialog')
      })
      expect(outside).toBe(false)
    }
  })

  test('asks for the typed phrase before the shutdown', async ({ page }) => {
    const { mutations } = await mockOperations(page)
    await openOperations(page)

    await page.locator('[data-process-action="shutdown"]').click()
    const dialog = page.getByRole('dialog', { name: 'Shut down the process' })
    const confirm = dialog.getByRole('button', { name: 'Shut down' })
    await expect(confirm).toBeDisabled()
    await dialog.getByLabel('Type shutdown to confirm').fill('shutdown')
    await page.keyboard.press('Tab')
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    await page.keyboard.press('Tab')
    await expect(confirm).toBeFocused()
    await page.keyboard.press('Enter')

    await expect.poll(() => mutations.length).toBe(1)
    expect(mutations[0]).toEqual({ path: '/api/v1/process/shutdown', method: 'POST', body: expectedRevision })
    await expect(page.getByRole('status').filter({ hasText: 'Shutdown requested' }).first()).toBeVisible()
  })

  test('sets and resets the weight next to the connection limit', async ({ page }) => {
    const { mutations } = await mockOperations(page)
    await openOperations(page, 'Servers')

    await page.getByLabel('Weight of server primary in pool web-backends').fill('40')
    await page.getByRole('button', { name: 'Apply weight for server primary in pool web-backends' }).click()
    await expect.poll(() => mutations.length).toBe(1)
    await page.getByRole('button', { name: 'Reset weight for server primary in pool web-backends' }).click()
    await expect.poll(() => mutations.length).toBe(2)

    expect(mutations).toEqual([
      { path: '/api/v1/servers/weight', method: 'PUT', body: { ...primary, weight: 40, ...expectedRevision } },
      { path: '/api/v1/servers/weight', method: 'PUT', body: { ...primary, weight: null, ...expectedRevision } },
    ])
  })

  test('evacuates a server with force and reports what was left running', async ({ page }) => {
    const { mutations } = await mockOperations(page, (request) =>
      request.url().endsWith('/servers/evacuate')
        ? json({ outcome: 'evacuated', force: true, cancelledAndRedispatched: 2, forced: 1, leftRunning: { no_destination: 3 } })
        : undefined,
    )
    await openOperations(page, 'Servers')

    await page.getByLabel('Force evacuation: also cut requests that cannot move').check()
    await page.getByRole('button', { name: 'Evacuate server primary in pool web-backends' }).click()
    const dialog = page.getByRole('dialog', { name: 'Evacuate server primary' })
    await expect(dialog).toContainText('Force: on')
    await dialog.getByRole('button', { name: 'Evacuate and force' }).click()

    await expect.poll(() => mutations.length).toBe(1)
    expect(mutations[0]).toEqual({
      path: '/api/v1/servers/evacuate',
      method: 'POST',
      body: { ...primary, force: true, ...expectedRevision },
    })
    const outcome = page.getByRole('status').filter({ hasText: 'Evacuated server primary' }).first()
    await expect(outcome).toContainText('3 requests left running: no other server can take them now.')
  })

  test('filters servers and hides controls that cannot apply', async ({ page }) => {
    await mockOperations(page)
    await openOperations(page, 'Servers')
    const cards = page.locator('[data-server]')

    await expect(cards).toHaveCount(3)
    await expect(page.getByRole('button', { name: 'Refresh DNS of server static-1 in pool static' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Refresh DNS of server primary in pool web-backends' })).toHaveCount(0)
    await expect(page.getByRole('button', { name: /checks of server static-1/ })).toHaveCount(0)

    await page.getByRole('button', { name: 'Problems only' }).click()
    await expect(cards).toHaveCount(1)
    await page.getByRole('button', { name: 'Problems only' }).click()
    await page.getByLabel('Search servers').fill('origin.example')
    await expect(cards).toHaveCount(1)
    await expect(cards.first()).toHaveAttribute('data-server', 'static-1')
  })

  test('lists in-flight requests with the filters the API defines', async ({ page }) => {
    const queries: string[] = []
    await mockOperations(page, (request) => {
      if (new URL(request.url()).pathname === '/api/v1/requests') queries.push(new URL(request.url()).search)
      return undefined
    })
    await openOperations(page, 'In-flight requests')

    await expect(page.locator('[data-request]')).toHaveCount(2)
    await expect(page.locator('[data-request]').first()).toHaveAttribute('data-request', 'rq-old')
    await page.getByLabel('Phase').selectOption('queued')
    await page.getByLabel('Older than (seconds)').fill('30')
    await page.getByRole('button', { name: 'Apply filters' }).click()
    await expect.poll(() => queries.at(-1)).toBe('?phase=queued&min_age_ms=30000&limit=100')
  })

  test('lists the registrations and the capabilities', async ({ page }) => {
    await mockOperations(page)
    await openOperations(page)
    await expect(page.getByRole('table', { name: 'Transport capabilities and supervision mode' })).toBeVisible()

    await page.getByRole('tab', { name: 'Registrations' }).click()
    await expect(page.getByRole('row', { name: /worker-a.*Active/ })).toBeVisible()
    await expect(page.getByRole('row', { name: /worker-b.*Shadowed/ })).toBeVisible()
  })

  test('shows the restart indicator as a banner with its reasons', async ({ page }) => {
    const reasons = [{ code: 'listener_ipv6_only', path: '/listeners/web', summary: 'ipv6_only changed on a kept address' }]
    await mockOperations(page, (request) =>
      new URL(request.url()).pathname === '/api/v1/status'
        ? json({ ...managementStatus(), restartRequired: { required: true, candidateRevision: 'next', since: 1_750_000_000_000, reasons } })
        : undefined,
    )
    await openOperations(page)

    const banner = page.getByRole('status').filter({ hasText: 'Restart required.' })
    await expect(banner).toBeVisible()
    await expect(banner).toContainText('/listeners/web')
    await expect(banner).toContainText('ipv6_only changed on a kept address')
  })
})
