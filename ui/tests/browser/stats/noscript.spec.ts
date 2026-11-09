import { expect, test } from '@playwright/test'
import type { Browser, Page } from '@playwright/test'

import { PAGE, serveStats } from './support'

// The page listener fills the shell's tokens before sending it. The preview server sends the build as it is, so this
// does what the listener does: the same tokens, a table like the server-rendered one, with the prefix empty.
const TABLE = '<table><thead><tr><th>Pool</th><th>Server</th></tr></thead><tbody>' +
  '<tr><td>web</td><td>origin-a</td></tr><tr><td>api</td><td>origin-c</td></tr></tbody></table>'

async function serveShell(page: Page): Promise<void> {
  await page.route(`**${PAGE}`, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    const response = await route.fetch()
    const body = (await response.text())
      .replaceAll('__stats_table__', TABLE)
      .replaceAll('__stats_refresh_s__', '60')
      .replaceAll('__stats_refresh_ms__', '60000')
      .replaceAll('/__stats_prefix__', '')
    return route.fulfill({ response, body })
  })
}

const withoutScript = async (browser: Browser) => {
  const context = await browser.newContext({ javaScriptEnabled: false, baseURL: 'http://127.0.0.1:4174' })
  return { context, page: await context.newPage() }
}

test.describe('stats page without script', () => {
  test('shows the server-rendered table and refreshes by itself', async ({ browser }) => {
    const { context, page } = await withoutScript(browser)
    await serveShell(page)
    await page.goto(PAGE)
    await expect(page.getByRole('heading', { level: 1, name: 'OxiRoute statistics' })).toBeVisible()
    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    await expect(table.getByRole('row')).toHaveCount(3)
    await expect(table).toContainText('origin-a')
    await expect(page.locator('meta[http-equiv="refresh"]')).toHaveAttribute('content', '60')
    await expect(page.locator('#app')).toBeEmpty()
    await context.close()
  })

  test('with script the client replaces the fallback: no second table, no refresh meta', async ({ page }) => {
    await serveStats(page)
    await serveShell(page)
    await page.goto(PAGE)
    await expect(page.getByRole('status').first()).toHaveAttribute('data-status', 'live')
    await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1)
    expect(await page.locator('meta[http-equiv="refresh"]').count()).toBe(0)
    expect(await page.locator('noscript').evaluateAll((nodes) => nodes.every((node) => node.childElementCount === 0))).toBe(true)
  })
})
