import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import {
  installApiMock,
  json,
  managementEventPage,
  managementMonitoring,
  requestBody,
  requestPath,
  shutdownStream,
  unlock,
} from './support'
import { THEMES, expectNoAxeViolations, expectNoPageScroll, trackExternalRequests, useTheme } from './support/lists'
import { managementGeneration, managementTlsInventory } from '../../src/test/managementFixtures'

const NOW = new Date('2026-07-12T00:00:00Z')

async function mockCertificates(page: Page, inventory = managementTlsInventory()): Promise<Array<{ path: string; body: unknown }>> {
  const posts: Array<{ path: string; body: unknown }> = []
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    if (request.method() === 'POST') {
      posts.push({ path, body: requestBody(request) })
      return json({ certificate: 'managed-edge', outcome: 'renewed', diskRevision: 'd', activeRevision: 'active-revision' })
    }
    if (path === '/api/v1/monitoring') return managementMonitoring()
    if (path === '/api/v1/tls') return json(inventory)
    if (path === '/api/v1/generations') return json(managementGeneration())
    if (path === '/api/v2/events') return json(managementEventPage())
    if (path === '/api/v2/events/stream') return shutdownStream()
    return undefined
  })
  await page.clock.setFixedTime(NOW)
  return posts
}

test.describe('certificates workspace browser gates', () => {
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
    const external = trackExternalRequests(page)
    await mockCertificates(page, inventory)

    await page.goto('/#/certificates')
    await unlock(page)
    await expect(page.getByRole('heading', { name: 'Configured certificates' })).toBeVisible()
    await expect(page.getByRole('main')).toContainText('managed-edge')
    await expect(page.getByRole('main')).toContainText('tls_alpn01')
    const text = await page.locator('body').innerText()
    expect(text).not.toContain('PRIVATE-KEY-MATERIAL')
    expect(text).not.toContain('account/secret')
    expect(text).not.toContain('order/secret')
    expect(text).not.toContain('challenge-secret')
    expect(external).toEqual([])
  })

  test('shows UTC times, relative expiry and a warning badge with word and shape', async ({ page }) => {
    await mockCertificates(page)
    await page.goto('/#/certificates')
    await unlock(page)

    const card = page.locator('.ox-cert', { hasText: 'managed-edge' })
    await expect(card).toContainText('2026-08-01 00:00:00 UTC, expires in 20 days')
    const badge = card.locator('.ox-badge', { hasText: 'Expires soon' })
    await expect(badge).toBeVisible()
    await expect(badge.locator('svg')).toBeVisible()
    await expect(page.getByText('2 certificates')).toBeVisible()
    await expect(page.getByText('Management token required')).toHaveCount(0)
  })

  test('asks in a dialog that names the certificate before sending the active revision', async ({ page }) => {
    const posts = await mockCertificates(page)
    await page.goto('/#/certificates')
    await unlock(page)

    const card = page.locator('.ox-cert', { hasText: 'managed-edge' })
    await card.getByRole('button', { name: 'Renew now' }).click()
    const dialog = page.getByRole('dialog', { name: 'Renew managed-edge?' })
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: 'Cancel' }).click()
    expect(posts).toEqual([])

    await card.getByRole('button', { name: 'Renew now' }).click()
    await page.getByRole('dialog', { name: 'Renew managed-edge?' }).getByRole('button', { name: 'Renew now' }).click()
    await expect(page.getByText('Renew managed-edge completed with outcome renewed.')).toBeVisible()
    expect(posts).toEqual([{ path: '/api/v1/tls/renew', body: { expectedActiveRevision: 'active-revision', certificate: 'managed-edge' } }])
  })

  test('lists certificate jobs in a keyboard-reachable table', async ({ page }) => {
    await mockCertificates(page)
    await page.goto('/#/certificates')
    await unlock(page)

    const jobs = page.getByRole('region', { name: 'Certificate jobs table' })
    await expect(jobs.getByRole('table')).toContainText('Certificate renewal')
    await jobs.focus()
    await expect(jobs).toBeFocused()
    await expectNoPageScroll(page)
  })

  for (const theme of THEMES) {
    test(`has no accessibility violations and no page scroll in the ${theme} theme`, async ({ page }) => {
      await useTheme(page, theme)
      await mockCertificates(page)
      await page.goto('/#/certificates')
      await unlock(page)
      await expect(page.getByRole('heading', { name: 'Configured certificates' })).toBeVisible()
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
      await expectNoAxeViolations(page)
      await expectNoPageScroll(page)
    })
  }
})
