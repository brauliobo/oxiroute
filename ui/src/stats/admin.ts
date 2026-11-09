import type { AdministrativeState } from './generated/statsPage'

// The administration form of the page: `POST <page path>` with urlencoded `generation_revision` (the active revision the
// viewer saw), `pool`, `server` and `state`. The server answers 204, or an error object; it also requires a loopback
// client, a loopback Host and the page's own Origin, which a same-origin fetch from the page satisfies.

export const ADMIN_STATES: ReadonlyArray<{ state: AdministrativeState; label: string; past: string }> = [
  { state: 'ready', label: 'Ready', past: 'ready' },
  { state: 'drain', label: 'Drain', past: 'draining' },
  { state: 'maintenance', label: 'Maintenance', past: 'in maintenance' },
]

export interface AdminRequest {
  revision: string
  pool: string
  server: string
  state: AdministrativeState
}
export type AdminOutcome = { ok: true } | { ok: false; status?: number; message: string }

const MESSAGES: Readonly<Record<number, string>> = {
  403: 'The server refused: administration is allowed only from this machine on this page.',
  409: 'The configuration changed since this page loaded. The page was refreshed: review it and try again.',
}

async function failure(response: Response): Promise<AdminOutcome> {
  const body = await response.json().catch(() => undefined)
  const message = MESSAGES[response.status] ?? body?.error?.message ?? `The request failed (HTTP ${response.status}).`
  return { ok: false, status: response.status, message }
}

export async function postAdminState(pathname: string, request: AdminRequest, fetcher: typeof fetch = fetch): Promise<AdminOutcome> {
  const body = new URLSearchParams({
    generation_revision: request.revision, pool: request.pool, server: request.server, state: request.state,
  })
  try {
    const response = await fetcher(pathname, { method: 'POST', body, credentials: 'same-origin', cache: 'no-store' })
    return response.ok ? { ok: true } : await failure(response)
  } catch {
    return { ok: false, message: 'The server could not be reached.' }
  }
}
