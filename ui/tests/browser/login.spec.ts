import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { MANAGEMENT_TOKEN, dashboardResponse, installApiMock, json, requestPath, unlock } from './support'

async function mockServer(page: Page): Promise<string[]> {
  const authorizations: string[] = []
  await installApiMock(page, (request) => {
    authorizations.push(request.headers()['authorization'] ?? 'none')
    if (request.headers()['authorization'] !== `Bearer ${MANAGEMENT_TOKEN}`) {
      return json({ error: { code: 'unauthorized', message: 'management bearer token required' } }, 401)
    }
    return dashboardResponse(requestPath(request))
  })
  return authorizations
}

test.describe('console login', () => {
  test('sends no request before a token exists and asks for it once', async ({ page }) => {
    const authorizations = await mockServer(page)
    await page.goto('/#/overview')
    await expect(page.getByRole('heading', { name: 'Management token' })).toBeVisible()
    await expect(page.getByText('No management token.')).toBeVisible()
    await expect(page.locator('input[type="password"]')).toHaveCount(1)
    await expect(page.getByRole('navigation')).toHaveCount(0)
    expect(authorizations).toEqual([])
  })

  test('tells a rejected token from a missing one and forgets it', async ({ page }) => {
    await mockServer(page)
    await page.goto('/#/overview')
    await unlockWith(page, 'wrong-token')
    await expect(page.getByRole('alert')).toContainText('Management token rejected')
    await expect(page.getByText('No management token.')).toHaveCount(0)
    expect(await page.evaluate(() => sessionStorage.length)).toBe(0)
    await expect(page.getByLabel('Bearer token')).toHaveValue('')
  })

  test('keeps the token for the tab session only and clears it on sign out', async ({ page }) => {
    const authorizations = await mockServer(page)
    await page.goto('/#/overview')
    await unlock(page)
    await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
    expect(await page.evaluate(() => localStorage.length)).toBe(0)
    expect(await page.evaluate(() => Object.values(sessionStorage).includes('browser-test-management-token'))).toBe(true)
    expect(authorizations.every((value) => value === `Bearer ${MANAGEMENT_TOKEN}`)).toBe(true)
    expect(page.url()).not.toContain(MANAGEMENT_TOKEN)

    await page.reload()
    await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
    await expect(page.getByLabel('Bearer token')).toHaveCount(0)

    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page.getByText('No management token.')).toBeVisible()
    expect(await page.evaluate(() => sessionStorage.length)).toBe(0)
    const before = authorizations.length
    await page.reload()
    await expect(page.getByLabel('Bearer token')).toBeVisible()
    expect(authorizations).toHaveLength(before)
  })
})

async function unlockWith(page: Page, token: string): Promise<void> {
  await page.getByLabel('Bearer token').fill(token)
  await page.getByRole('button', { name: 'Unlock console' }).click()
}
