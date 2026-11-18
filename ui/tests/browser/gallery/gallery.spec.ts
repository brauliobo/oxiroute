import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

const THEMES = ['light', 'dark'] as const
const DENSITIES = ['compact', 'comfortable'] as const

for (const theme of THEMES) {
  for (const density of DENSITIES) {
    test.describe(`gallery ${theme} ${density}`, () => {
      test.beforeEach(async ({ page }) => {
        await page.goto(`/gallery.html?theme=${theme}&density=${density}`)
        await expect(page.locator('html')).toHaveAttribute('data-density', density)
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
        await expect(page.getByRole('heading', { level: 1, name: 'Design gallery' })).toBeVisible()
      })

      test('renders every primitive as designed', async ({ page }) => {
        await expect(page).toHaveScreenshot(`${theme}-${density}.png`, { fullPage: true })
      })

      test('has no accessibility violations', async ({ page }) => {
        const results = await new AxeBuilder({ page }).analyze()
        expect(results.violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)).toEqual([])
      })
    })
  }
}

test.describe('gallery behaviour', () => {
  test('opens the dialog modally and closes it with Escape, giving focus back', async ({ page }) => {
    await page.goto('/gallery.html')
    const opener = page.getByRole('button', { name: 'Open dialog' })
    await opener.click()
    const dialog = page.getByRole('dialog', { name: 'Confirm administration' })
    await expect(dialog).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(opener).toBeFocused()
  })

  test('moves between tabs with the arrow keys', async ({ page }) => {
    await page.goto('/gallery.html')
    await page.getByRole('tab', { name: 'General' }).focus()
    await page.keyboard.press('ArrowRight')
    await expect(page.getByRole('tab', { name: 'Pools' })).toHaveAttribute('aria-selected', 'true')
    await expect(page.getByRole('tab', { name: 'Pools' })).toBeFocused()
    await expect(page.getByRole('tabpanel')).toContainText('pools tab')
  })

  test('switches the theme and the density from the header', async ({ page }) => {
    await page.goto('/gallery.html')
    await page.getByLabel('Theme').selectOption('dark')
    await page.getByLabel('Density').selectOption('comfortable')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    await expect(page.locator('html')).toHaveAttribute('data-density', 'comfortable')
    const height = await page.getByRole('button', { name: 'Default' }).evaluate((button) => button.getBoundingClientRect().height)
    expect(height).toBeGreaterThanOrEqual(44)
  })
})
