import AxeBuilder from '@axe-core/playwright'
import { expect } from '@playwright/test'
import type { Page } from '@playwright/test'

import { origins } from '../ports'

export const THEMES = ['light', 'dark'] as const
export type ThemeName = (typeof THEMES)[number]

export function useTheme(page: Page, theme: ThemeName): Promise<void> {
  return page.addInitScript((value) => localStorage.setItem('oxiroute.theme', value), theme)
}

export function trackExternalRequests(page: Page): string[] {
  const external: string[] = []
  page.on('request', (request) => {
    if (!request.url().startsWith(origins.console) && !request.url().startsWith('data:')) external.push(request.url())
  })
  return external
}

export async function expectNoAxeViolations(page: Page): Promise<void> {
  const { violations } = await new AxeBuilder({ page }).analyze()
  expect(violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(' ')).join(' | ')}`)).toEqual([])
}

export async function expectNoPageScroll(page: Page): Promise<void> {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
}
