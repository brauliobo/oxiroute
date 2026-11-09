import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { HOSTILE_POOL, HOSTILE_SERVER, dashboardDocument, serve } from './seed'

// The built page, seeded by route interception, under the strict policy the page listener sends. Screenshots are
// only taken when STATS_SHOTS names a directory; they come from the invented data of seed.ts.
const shots = process.env.STATS_SHOTS
const shoot = async (page: Page, name: string) => { if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true }) }

const watchProblems = async (page: Page) => {
  const problems: string[] = []
  page.on('console', (message) => { if (message.type() === 'error' || message.type() === 'warning') problems.push(message.text()) })
  page.on('pageerror', (error) => problems.push(error.message))
  await page.exposeFunction('reportViolation', (directive: string) => problems.push(`csp: ${directive}`))
  await page.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      void (window as unknown as { reportViolation(directive: string): void }).reportViolation(event.violatedDirective)
    })
  })
  return problems
}

const growing = Array.from({ length: 6 }, (_, index) => dashboardDocument(index))
const serverRow = (page: Page, name: string) => page.locator('tbody.ox-row', { has: page.getByRole('button', { name, exact: true }) })

test.describe('stats dashboard on a wide screen', () => {
  test('shows every view, with statuses as words and shapes, and raises no console or policy errors', async ({ page }) => {
    const problems = await watchProblems(page)
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await expect(page.getByRole('status')).toHaveAttribute('data-status', 'live')

    const overview = page.getByLabel('Overview')
    await expect(overview).toContainText('6.3.0')
    await expect(overview).toContainText('Generation')
    await expect(overview.locator('.ox-tile', { hasText: 'Certificates' })).toContainText('EXPIRES SOON')
    await expect(page.getByRole('table', { name: /^Listeners/ })).toBeVisible()
    await expect(page.getByRole('table', { name: /^Routes/ })).toBeVisible()
    await expect(page.getByRole('table', { name: /^Pools with their servers/ })).toBeVisible()
    for (const heading of ['Generation 4', 'Process', 'Certificates', 'Warnings']) {
      await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible()
    }

    const badges = page.locator('tbody.ox-row .ox-badge')
    expect(await badges.allInnerTexts()).toEqual(expect.arrayContaining(['UP', 'DOWN', 'DRAIN', 'MAINT', 'EJECTED', 'UNCHECKED', 'FAILED']))
    for (const badge of await badges.all()) await expect(badge.locator('svg[aria-hidden="true"]')).toBeVisible()
    const shapes = await page.locator('tbody.ox-row .ox-badge').evaluateAll((items) =>
      Object.fromEntries(items.map((item) => [item.textContent, item.querySelector('path')!.getAttribute('d')])))
    expect(new Set([shapes.UP, shapes.DRAIN, shapes.DOWN, shapes.MAINT, shapes.UNCHECKED]).size).toBe(5)

    await expect(serverRow(page, 'origin-b').locator('[data-label="Downtime"]')).toContainText('2m 5s')
    await expect(serverRow(page, 'origin-a').getByText('3 / 100')).toBeVisible()
    await expect(serverRow(page, 'origin-a').locator('[data-label="Req/s"]')).toHaveText('—')
    expect(problems).toEqual([])
    await shoot(page, 'desktop')
  })

  test('has sticky column headers, a caption and header scope on every table', async ({ page }) => {
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    const header = page.getByRole('table', { name: /^Pools with their servers/ }).locator('thead th').first()
    await expect(header).toHaveCSS('position', 'sticky')
    const scopes = await page.locator('thead th').evaluateAll((items) => items.map((item) => item.getAttribute('scope')))
    expect(scopes.length).toBeGreaterThan(20)
    expect(scopes.every((scope) => scope === 'col')).toBe(true)
    const rows = await page.locator('tbody.ox-row > tr:first-child > th').evaluateAll((items) => items.map((item) => item.getAttribute('scope')))
    expect(rows.every((scope) => scope === 'row')).toBe(true)
  })

  test('opens details with the keyboard and keeps them open across polls and reloads', async ({ page }) => {
    await serve(page, growing)
    await page.goto('/stats.html')
    const toggle = page.getByRole('button', { name: 'origin-a', exact: true })
    await toggle.focus()
    await expect(toggle).toHaveAttribute('aria-expanded', 'false')
    await page.keyboard.press('Enter')
    await expect(toggle).toHaveAttribute('aria-expanded', 'true')
    const details = page.locator('#' + (await toggle.getAttribute('aria-controls')))
    await expect(details).toContainText('198.51.100.11:8080')
    await expect(details).toContainText('Attempts by outcome')
    await expect(details).toContainText('Request duration')
    await expect(details.getByText('Not reported by this build')).toBeVisible()
    await shoot(page, 'desktop-details')
    await expect(page).toHaveURL(/open=server%3Aweb%2Forigin-a/)
    await expect(page.getByRole('status')).toHaveAttribute('data-status', 'live')
    await page.waitForTimeout(1500)
    await expect(toggle).toHaveAttribute('aria-expanded', 'true')
    await page.reload()
    await expect(page.getByRole('button', { name: 'origin-a', exact: true })).toHaveAttribute('aria-expanded', 'true')
    await page.getByRole('button', { name: 'origin-a', exact: true }).press('Space')
    await expect(page.getByRole('button', { name: 'origin-a', exact: true })).toHaveAttribute('aria-expanded', 'false')
  })

  test('folds a pool and opens the details of a pool and a listener', async ({ page }) => {
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await page.getByRole('button', { name: 'web', exact: true }).click()
    await expect(page.getByRole('button', { name: 'origin-a', exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'web', exact: true }).click()
    await expect(page.getByRole('button', { name: 'origin-a', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Pool details' }).first().click()
    await expect(page.locator('.ox-details').first()).toContainText('Selections with no server available')
    await page.getByRole('button', { name: 'public-http', exact: true }).click()
    await expect(page.getByText('socket:192.0.2.10:8080')).toBeVisible()
    await expect(page.locator('.ox-routes a', { hasText: 'web' })).toBeVisible()
  })

  test('sorts by a header, puts the sort in the address and restores it', async ({ page }) => {
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    const names = () => page.locator('tbody.ox-row[data-row^="server:web"] .ox-label').allInnerTexts()
    expect(await names()).toEqual(['origin-a', 'origin-b', 'origin-c', 'origin-d', 'origin-e', 'origin-f'])
    const header = page.getByRole('columnheader', { name: /^Status/ })
    await header.getByRole('button').click()
    await expect(header).toHaveAttribute('aria-sort', 'ascending')
    await expect(page).toHaveURL(/sort=status/)
    expect((await names())[0]).toBe('origin-b')
    await page.reload()
    await expect(page.getByRole('columnheader', { name: /^Status/ })).toHaveAttribute('aria-sort', 'ascending')
    expect((await names())[0]).toBe('origin-b')
  })

  test('shows rates and sparklines once there are two samples, and marks a status change in words', async ({ page }) => {
    await serve(page, [...growing.slice(0, 3), ...growing.slice(3).map((wire) => ({
      ...wire, pools: wire.pools.map((pool, index) => (index === 0 ? { ...pool, servers: pool.servers.map((server) => (
        server.name === 'origin-a' ? { ...server, status: 'down' as const } : server)) } : pool)),
    }))])
    await page.goto('/stats.html?interval=1000')
    const row = serverRow(page, 'origin-a')
    await expect(row.locator('[data-label="Req/s"]')).toContainText('100/s')
    await expect(row.locator('[data-label="Req/s"] svg[role="img"]')).toBeVisible()
    await expect(row.locator('[data-label="Errors"]')).toContainText('5/s')
    await expect(row.locator('[data-label="Out"]')).toContainText('49 KiB/s')
    await expect(row.getByText('status changed')).toBeVisible({ timeout: 10_000 })
    await expect(row.locator('[data-label="Status"]')).toContainText('DOWN')
    await shoot(page, 'desktop-rates')
  })

  test('renders hostile names as text and runs nothing', async ({ page }) => {
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await expect(page.getByRole('button', { name: HOSTILE_POOL, exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: HOSTILE_SERVER, exact: true })).toBeVisible()
    await page.getByRole('button', { name: HOSTILE_SERVER, exact: true }).click()
    expect(await page.evaluate(() => (window as unknown as { xss?: number }).xss)).toBeUndefined()
    expect(await page.locator('img[src="x"]').count()).toBe(0)
  })

  test('fits the table in a 1100 px window without scrolling sideways', async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 900 })
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await expect(page.getByRole('table', { name: /^Pools with their servers/ })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0)
  })

  test('follows a dark colour scheme', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'dark' })
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await expect(page.getByRole('table', { name: /^Pools with their servers/ })).toBeVisible()
    await shoot(page, 'desktop-dark')
  })
})

test.describe('stats dashboard on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })

  test('collapses tables to labelled cards without sideways scrolling', async ({ page }) => {
    await serve(page, growing)
    await page.goto('/stats.html')
    const row = serverRow(page, 'origin-a')
    await expect(row).toBeVisible()
    await expect(page.locator('table.ox-servers thead')).toBeHidden()
    const label = await row.locator('[data-label="Sessions"]').evaluate((cell) => getComputedStyle(cell, '::before').content)
    expect(label).toBe('"Sessions"')
    await expect(row.locator('[data-label="Status"]')).toContainText('UP')
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0)
    await shoot(page, 'mobile')
  })

  test('opens a card, and the details stay inside the screen', async ({ page }) => {
    await serve(page, [dashboardDocument(0)])
    await page.goto('/stats.html')
    await page.getByRole('button', { name: 'origin-b', exact: true }).tap()
    await expect(page.locator('.ox-details').first()).toContainText('198.51.100.12:8080')
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0)
    await shoot(page, 'mobile-details')
  })
})
