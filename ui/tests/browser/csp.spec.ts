import { expect, test } from '@playwright/test'

import { dashboardResponse, installApiMock, requestPath, unlock } from './support'

// `vite preview` serves the build with a strict policy (vite.config.ts). Static and string style attributes are
// blocked by it; Vapor applies object bindings through the CSSOM, so geometry comes from custom properties.
test.describe('strict content security policy', () => {
  test('renders runtime-sized meters and the topology layout without style-src violations', async ({ page }) => {
    const violations: string[] = []
    await page.exposeFunction('reportViolation', (directive: string) => violations.push(directive))
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (event) => {
        void (window as unknown as { reportViolation(directive: string): void }).reportViolation(event.violatedDirective)
      })
    })
    await installApiMock(page, (request) => dashboardResponse(requestPath(request)))

    const response = await page.goto('/#/overview')
    const policy = response?.headers()['content-security-policy'] ?? ''
    expect(policy).toContain("style-src 'self'")
    expect(policy).toContain("media-src 'self'")
    expect(policy).not.toMatch(/blob:|data:|unsafe-/)
    await unlock(page)
    await expect(page.locator('.ox-node').first()).toBeVisible()

    const loadWidth = await page.locator('.ox-loads .ox-meter-fill').first().evaluate((bar) => bar.getBoundingClientRect().width)
    const memory = page.getByRole('meter', { name: 'Host memory used' })
    const memoryWidth = await memory.locator('.ox-meter-fill').evaluate((bar) => bar.getBoundingClientRect().width)
    const memoryTrackWidth = await memory.evaluate((track) => track.getBoundingClientRect().width)
    expect(loadWidth).toBeGreaterThan(0)
    expect(memoryWidth / memoryTrackWidth).toBeCloseTo(0.75, 1)
    const nodeLeft = await page.locator('.ox-node-slot').first().evaluate((node) => getComputedStyle(node).left)
    expect(nodeLeft).toMatch(/^\d+(\.\d+)?px$/)
    await page.locator('[data-node-id^="upstream_pool"]').click()
    await expect(page.getByLabel('Topology node inspector')).toBeVisible()
    expect(violations).toEqual([])
  })
})
