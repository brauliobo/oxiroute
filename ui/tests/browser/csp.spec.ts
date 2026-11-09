import { expect, test } from '@playwright/test'

import { dashboardResponse, installApiMock, requestPath } from './support'

// `vite preview` serves the build with a strict policy (vite.config.ts). Static and string style attributes are
// blocked by it; Vapor applies object bindings through the CSSOM, so geometry comes from custom properties.
test.describe('strict content security policy', () => {
  test('renders runtime-sized bars and the topology layout without style-src violations', async ({ page }) => {
    const violations: string[] = []
    await page.exposeFunction('reportViolation', (directive: string) => violations.push(directive))
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (event) => {
        void (window as unknown as { reportViolation(directive: string): void }).reportViolation(event.violatedDirective)
      })
    })
    await installApiMock(page, (request) => dashboardResponse(requestPath(request)))

    const response = await page.goto('/#/overview')
    expect(response?.headers()['content-security-policy']).toContain("style-src 'self'")
    await expect(page.locator('.topology-node').first()).toBeVisible()

    const loadWidth = await page.locator('.load-track span').first().evaluate((bar) => bar.getBoundingClientRect().width)
    const memoryWidth = await page.locator('.memory-track span').evaluate((bar) => bar.getBoundingClientRect().width)
    const memoryTrackWidth = await page.locator('.memory-track').evaluate((track) => track.getBoundingClientRect().width)
    expect(loadWidth).toBeGreaterThan(0)
    expect(memoryWidth / memoryTrackWidth).toBeCloseTo(0.75, 1)
    const nodeLeft = await page.locator('.topology-node').first().evaluate((node) => getComputedStyle(node).left)
    expect(nodeLeft).toMatch(/^\d+(\.\d+)?px$/)
    expect(violations).toEqual([])
  })
})
