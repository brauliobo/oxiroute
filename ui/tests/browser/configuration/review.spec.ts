import { expect, test } from '@playwright/test'

import { installConfigApi, openConfiguration, themes } from './support'

for (const theme of themes) {
  test.describe(`save review (${theme})`, () => {
    test.beforeEach(async ({ page }) => {
      await installConfigApi(page)
      await openConfiguration(page, theme)
      await page.locator('[data-field="version"] input').fill('2')
      await page.getByRole('button', { name: 'Validate candidate' }).click()
      await expect(page.locator('.diagnostic-summary')).toHaveText('Validated: no diagnostics')
    })

    test('shows the changed lines of the rendered configuration with words and symbols', async ({ page }) => {
      await page.getByRole('button', { name: 'Review save' }).click()

      const dialog = page.getByRole('dialog', { name: 'Save review' })
      await expect(dialog).toBeVisible()
      await expect(dialog.locator('.ox-diff-summary')).toHaveText('1 line added (+), 1 line removed (-)')
      const removed = dialog.locator('.ox-diff-line[data-kind="removed"]')
      const added = dialog.locator('.ox-diff-line[data-kind="added"]')
      await expect(removed).toContainText('Removed: version 1')
      await expect(added).toContainText('Added: version 2')
      await expect(removed.locator('.ox-diff-mark')).toHaveText('-')
      await expect(added.locator('.ox-diff-mark')).toHaveText('+')
      await page.screenshot({ path: `/tmp/g1-cfg/review-${theme}.png` })
    })

    test('is operable by keyboard: focus lands inside, Tab never reaches the page behind, Escape closes and returns focus', async ({ page }) => {
      const opener = page.getByRole('button', { name: 'Review save' })
      await opener.focus()
      await page.keyboard.press('Enter')

      const dialog = page.getByRole('dialog', { name: 'Save review' })
      await expect(dialog).toBeVisible()
      await expect(dialog.getByRole('button', { name: 'Continue editing' })).toBeFocused()

      for (let press = 0; press < 6; press++) {
        await page.keyboard.press('Tab')
        expect(await page.evaluate(() => document.activeElement === document.body || document.activeElement?.closest('dialog') != null)).toBe(true)
      }

      await page.keyboard.press('Escape')
      await expect(dialog).toBeHidden()
      await expect(opener).toBeFocused()
    })
  })
}
