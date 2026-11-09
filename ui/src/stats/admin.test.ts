import { describe, expect, it, vi } from 'vitest'
import { postAdminState } from './admin'

const request = { revision: 'rev-1', pool: 'web', server: 'origin-a', state: 'drain' as const }
const answer = (status: number, body?: unknown) =>
  vi.fn<typeof fetch>(async () => new Response(body === undefined ? null : JSON.stringify(body), { status }))

describe('postAdminState', () => {
  it('posts the page form urlencoded to the page itself, same origin', async () => {
    const fetcher = answer(204)
    expect(await postAdminState('/haproxy', request, fetcher)).toEqual({ ok: true })
    const [url, init] = fetcher.mock.calls[0]!
    expect(url).toBe('/haproxy')
    expect(init).toMatchObject({ method: 'POST', credentials: 'same-origin', cache: 'no-store' })
    expect(init!.body).toBeInstanceOf(URLSearchParams)
    expect(Object.fromEntries(init!.body as URLSearchParams)).toEqual({
      generation_revision: 'rev-1', pool: 'web', server: 'origin-a', state: 'drain',
    })
  })

  it('encodes hostile names as data', async () => {
    const fetcher = answer(204)
    await postAdminState('/haproxy', { ...request, pool: 'a&state=ready', server: 'x=y\n' }, fetcher)
    const body = fetcher.mock.calls[0]![1]!.body as URLSearchParams
    expect(body.get('pool')).toBe('a&state=ready')
    expect(body.toString()).toContain('pool=a%26state%3Dready')
  })

  it('reports a changed generation as a reason to refresh and retry', async () => {
    const outcome = await postAdminState('/haproxy', request, answer(409, { error: { code: 'x', message: 'changed' } }))
    expect(outcome).toMatchObject({ ok: false, status: 409 })
    expect(outcome.ok ? '' : outcome.message).toContain('changed since this page loaded')
  })

  it('explains a refusal and shows the server message of other errors', async () => {
    const refused = await postAdminState('/haproxy', request, answer(403, { error: { code: 'admin_forbidden', message: 'no' } }))
    expect(refused.ok ? '' : refused.message).toContain('only from this machine')
    const missing = await postAdminState('/haproxy', request,
      answer(404, { error: { code: 'server_not_found', message: 'upstream server was not found' } }))
    expect(missing).toEqual({ ok: false, status: 404, message: 'upstream server was not found' })
    const opaque = await postAdminState('/haproxy', request, answer(502))
    expect(opaque).toEqual({ ok: false, status: 502, message: 'The request failed (HTTP 502).' })
  })

  it('reports an unreachable server', async () => {
    const fetcher = vi.fn<typeof fetch>(async () => { throw new TypeError('Failed to fetch') })
    expect(await postAdminState('/haproxy', request, fetcher)).toEqual({
      ok: false, message: 'The server could not be reached.',
    })
  })
})
