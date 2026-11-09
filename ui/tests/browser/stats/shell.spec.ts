import { expect, test } from '@playwright/test'

import { wireDocument } from '../../../src/stats/fixtures'

// `vite preview --config vite.stats.config.ts` serves dist-stats/ under the policy the page listener sends. The page
// is at /stats.html here, so its snapshot is /stats.html/snapshot.json (<page path>/snapshot.json).
test.describe('stats page shell', () => {
  test('loads under the strict CSP, shows the live summary and raises no console errors', async ({ page }) => {
    const problems: string[] = []
    page.on('console', (message) => { if (message.type() === 'error' || message.type() === 'warning') problems.push(message.text()) })
    page.on('pageerror', (error) => problems.push(error.message))
    await page.exposeFunction('reportViolation', (directive: string) => problems.push(`csp: ${directive}`))
    await page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (event) => {
        void (window as unknown as { reportViolation(directive: string): void }).reportViolation(event.violatedDirective)
      })
    })
    const origins = new Set<string>()
    page.on('request', (request) => origins.add(new URL(request.url()).origin))
    await page.route('**/stats.html/snapshot.json', (route) => route.fulfill({
      contentType: 'application/json', headers: { 'Cache-Control': 'no-store' }, body: JSON.stringify(wireDocument()),
    }))

    const response = await page.goto('/stats.html')
    expect(response?.headers()['content-security-policy']).toContain("script-src 'self'")
    await expect(page.getByRole('status')).toHaveAttribute('data-status', 'live')
    await expect(page.getByText('2 of 3 servers up')).toBeVisible()
    expect([...origins]).toEqual(['http://127.0.0.1:4174'])
    expect(problems).toEqual([])
  })
})
