import { expect, test } from '@playwright/test'

import { installApiMock, requestPath, unlock } from './support'
import { demoDashboardResponse } from './support/dashboard'

test.describe('topology', () => {
  test.beforeEach(async ({ page }) => {
    await installApiMock(page, (request) => demoDashboardResponse(requestPath(request)))
    await page.goto('/#/overview')
    await unlock(page)
    await expect(page.locator('.ox-node').first()).toBeVisible()
  })

  test('every node is a keyboard stop whose accessible name contains its visible text', async ({ page }) => {
    const nodes = page.locator('.ox-node')
    const count = await nodes.count()
    expect(count).toBe(6)

    for (let index = 0; index < count; index += 1) {
      const node = nodes.nth(index)
      await node.focus()
      await expect(node).toBeFocused()
      const visible = ((await node.innerText()) ?? '').split('\n').map((line) => line.trim().toLowerCase()).filter(Boolean)
      const name = (await node.evaluate((element) => (element as HTMLElement).getAttribute('aria-label') ?? element.textContent ?? '')).toLowerCase()
      for (const text of visible) expect(name).toContain(text)
      await expect(page.getByRole('button', { name: new RegExp(visible[1]!.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i') }).first()).toBeVisible()
    }
  })

  test('Tab reaches the nodes and Enter and Space inspect the focused node', async ({ page }) => {
    const first = page.locator('.ox-node').first()
    await page.getByRole('heading', { name: 'Network topology' }).click()
    for (let presses = 0; presses < 12 && !(await first.evaluate((node) => node === document.activeElement)); presses += 1) {
      await page.keyboard.press('Tab')
    }
    await expect(first).toBeFocused()

    await page.keyboard.press('Enter')
    const inspector = page.getByLabel('Topology node inspector')
    await expect(inspector).toBeVisible()
    await expect(first).toHaveAttribute('aria-pressed', 'true')

    await page.keyboard.press('ArrowRight')
    const second = page.locator('.ox-node').nth(1)
    await expect(second).toBeFocused()
    await page.keyboard.press('Space')
    await expect(second).toHaveAttribute('aria-pressed', 'true')
    await expect(inspector.getByRole('heading', { level: 3 })).toHaveText(await second.locator('strong').innerText())

    await page.keyboard.press('Escape')
    await expect(inspector).toBeHidden()
    await expect(second).toBeFocused()
  })

  test('shows the inspector without covering the node it describes, as readable definition lists', async ({ page }) => {
    const pool = page.locator('[data-node-id^="upstream_pool"]')
    const endpoint = page.locator('[data-node-id^="endpoint"]')
    await pool.click()
    const inspector = page.getByLabel('Topology node inspector')
    await expect(inspector).toBeVisible()

    const covered = await page.evaluate(() => {
      const panel = document.querySelector('.ox-inspector')!.getBoundingClientRect()
      return [...document.querySelectorAll('.ox-node')].some((node) => {
        const box = node.getBoundingClientRect()
        return box.left < panel.right && box.right > panel.left && box.top < panel.bottom && box.bottom > panel.top
      })
    })
    expect(covered).toBe(false)
    await expect(endpoint).toBeVisible()
    await expect(inspector.locator('pre')).toHaveCount(0)
    await expect(inspector).toContainText('HTTP versions / Min')
    await expect(inspector).not.toContainText('httpVersions')
  })

  test('shows long node names in full', async ({ page }) => {
    const node = page.locator('[data-node-id^="listener:"]')
    await expect(node).toContainText('edge-public-https-listener-with-a-deliberately-long-name')
    const clipped = await node.locator('strong').evaluate((element) => element.scrollWidth > element.clientWidth)
    expect(clipped).toBe(false)
  })
})
