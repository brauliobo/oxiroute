import type { Page } from '@playwright/test'

import { wireDocument } from '../../../src/stats/fixtures'
import type { StatsDocument } from '../../../src/stats/generated/statsPage'

// `vite preview --config vite.stats.config.ts` serves dist-stats/ at /stats.html, so the page's snapshot is
// /stats.html/snapshot.json and its administration form posts to /stats.html (<page path>, like the page listener).
export const PAGE = '/stats.html'

export interface Served {
  /** Snapshot requests so far. */
  snapshots(): number
  /** Administration forms posted to the page, as decoded urlencoded fields. */
  posts: Array<Record<string, string>>
  /** From now on answers snapshot requests with this failure, or with the document again. */
  failWith(failure: ServeOptions['failure']): void
}

interface ServeOptions {
  document?: Partial<StatsDocument>
  /** Status and body for the next administration posts; 204 by default. */
  admin?: { status: number; body?: unknown }
  /** Answers every snapshot request with this status and JSON body instead of the document. */
  failure?: { status: number; body: unknown }
}

export async function serveStats(page: Page, options: ServeOptions = {}): Promise<Served> {
  let snapshots = 0
  let { failure } = options
  const posts: Array<Record<string, string>> = []
  await page.route(`**${PAGE}/snapshot.json`, async (route) => {
    snapshots += 1
    await route.fulfill({
      status: failure?.status ?? 200,
      contentType: 'application/json',
      headers: { 'Cache-Control': 'no-store' },
      body: JSON.stringify(failure?.body ?? wireDocument(options.document)),
    })
  })
  await page.route(`**${PAGE}`, async (route) => {
    const request = route.request()
    if (request.method() !== 'POST') return route.fallback()
    posts.push(Object.fromEntries(new URLSearchParams(request.postData() ?? '')))
    const { status, body } = options.admin ?? { status: 204 }
    return route.fulfill({ status, contentType: 'application/json', body: body === undefined ? '' : JSON.stringify(body) })
  })
  return { snapshots: () => snapshots, posts, failWith: (next) => { failure = next } }
}
