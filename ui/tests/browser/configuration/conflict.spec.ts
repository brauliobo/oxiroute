import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

import { json, requestPath } from '../support'
import { demoSnapshot, installConfigApi, openConfiguration, selectObject, themes } from './support'

function newerOnDisk() {
  const snapshot = demoSnapshot()
  snapshot.diskRevision = 'newer-disk-revision'
  snapshot.config.version = 2
  const store = snapshot.config.cache_stores[0]!
  if (store.type === 'memory') store.max_bytes = 134_217_728
  return snapshot
}

async function conflictedEditor(page: Page, theme: (typeof themes)[number]): Promise<void> {
  let conflicted = false
  await installConfigApi(page, (request) => {
    const path = requestPath(request)
    if (path === '/api/v1/config' && request.method() === 'PUT') {
      conflicted = true
      return json({
        error: { code: 'config_revision_conflict', message: 'the disk revision changed before the write' },
        diskRevision: 'newer-disk-revision',
        activeRevision: 'disk-revision',
        diagnostics: [],
      }, 409)
    }
    if (path === '/api/v1/config' && request.method() === 'GET' && conflicted) return json(newerOnDisk())
    return undefined
  })
  await openConfiguration(page, theme)
  await page.locator('[data-field="version"] input').fill('3')
  await selectObject(page, 'Cache stores / memory-responses')
  await page.locator('[data-field="cache_stores[].max_entries"] input').fill('500')
  await page.getByRole('button', { name: 'Validate candidate' }).click()
  await page.getByRole('button', { name: 'Review save' }).click()
  await page.getByRole('button', { name: 'Save canonical configuration' }).click()
  await expect(page.locator('.revision-banner.stale')).toBeVisible()
}

for (const theme of themes) {
  test.describe(`stale revision conflict (${theme})`, () => {
    test('explains the conflict once and reapplies non-colliding edits onto the new disk file', async ({ page }) => {
      await conflictedEditor(page, theme)
      await expect(page.locator('.revision-banner')).toHaveCount(1)
      await expect(page.locator('.revision-banner.stale')).toContainText('The file on disk changed while you were editing')
      await page.screenshot({ path: `/tmp/g1-cfg/conflict-${theme}.png`, fullPage: true })

      await page.getByRole('button', { name: 'Reload and reapply my edits' }).click()

      const report = page.locator('.reapply-report')
      await expect(report).toContainText('Reapplied 1 change; 1 change collided')
      await expect(report).toContainText('/cache_stores/memory-responses/max_entries')
      await expect(report).toContainText('/version')
      await expect(report).toContainText('disk 2, yours 3')
      await expect(page.locator('.revision-board')).toContainText('newer-disk')
      await selectObject(page, 'General')
      await expect(page.locator('[data-field="version"] input')).toHaveValue('2')
      await selectObject(page, 'Cache stores / memory-responses')
      await expect(page.locator('[data-field="cache_stores[].max_entries"] input')).toHaveValue('500')
      await expect(page.locator('[data-field="cache_stores[].max_bytes"] input')).toHaveValue('134217728')
      await page.screenshot({ path: `/tmp/g1-cfg/reapplied-${theme}.png`, fullPage: true })
    })

    test('discards the draft after a confirmation that names what is lost', async ({ page }) => {
      await conflictedEditor(page, theme)

      await page.getByRole('button', { name: 'Discard my edits and load disk' }).click()
      const confirmation = page.getByRole('dialog', { name: 'Discard your draft and load the disk file?' })
      await expect(confirmation).toContainText('/version')
      await expect(confirmation).toContainText('/cache_stores/memory-responses/max_entries')
      await confirmation.getByRole('button', { name: 'Discard changes' }).click()

      await expect(page.locator('.revision-banner.stale')).toBeHidden()
      await expect(page.locator('.reapply-report')).toHaveCount(0)
      await selectObject(page, 'General')
      await expect(page.locator('[data-field="version"] input')).toHaveValue('2')
    })
  })
}
