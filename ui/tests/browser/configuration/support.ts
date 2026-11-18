import { expect } from '@playwright/test'
import type { Page, Request } from '@playwright/test'

import type { CanonicalConfig, ConfigSnapshot } from '../../../src/config'
import { contractConfigSnapshot } from '../../../src/test/contractFixtures'
import { CONFIG_TOKEN, configValidation, installApiMock, json, requestBody, requestPath, shutdownStream, unlock } from '../support'
import type { ApiMockResponse } from '../support'

export const themes = ['light', 'dark'] as const

export function demoSnapshot(): ConfigSnapshot {
  const snapshot = contractConfigSnapshot()
  snapshot.diskRevision = 'disk-revision'
  snapshot.candidateRevision = 'disk-revision'
  snapshot.activeRevision = 'disk-revision'
  return snapshot
}

export type ConfigApiHandler = (request: Request) => ApiMockResponse | undefined

export async function installConfigApi(page: Page, handler: ConfigApiHandler = () => undefined): Promise<void> {
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    const custom = handler(request)
    if (custom) return custom
    if (path === '/api/v1/config' && request.method() === 'GET') return json(demoSnapshot())
    if (path === '/api/v1/config/validate') return json(configValidation(requestBody<{ config: CanonicalConfig }>(request).config))
    if (path === '/api/v2/events/stream') return shutdownStream()
    return undefined
  })
}

export async function openConfiguration(page: Page, theme: (typeof themes)[number] = 'dark'): Promise<void> {
  await page.addInitScript((value) => localStorage.setItem('oxiroute.theme', value), theme)
  await page.goto('/#/configuration')
  await unlock(page, CONFIG_TOKEN)
  await expect(page.locator('.revision-board')).toBeVisible()
}

export async function selectObject(page: Page, label: string, detail?: string): Promise<void> {
  const mobile = page.locator('#mobile-object-navigation')
  if (await mobile.isVisible()) return void (await mobile.selectOption({ label }))
  const rail = page.getByRole('navigation', { name: 'Configuration objects' })
  const name = label.split(' / ').at(-1)!
  await rail.getByRole('button', { name: new RegExp(`^${name} ${detail ?? ''}`) }).click()
}
