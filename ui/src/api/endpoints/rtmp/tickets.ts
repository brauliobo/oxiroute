import { isRecord, safeInteger } from '../../../valueGuards'
import type { TicketResponse } from '../../generated/controlPlane'
import { request } from '../../transport'
import { invalidPayload, mutationHeaders } from '../shared'

export type MediaTicket = TicketResponse

export const MAX_TICKET_TTL_SECONDS = 600
const TICKET_PATH = /^\/api\/v1\/rtmp\/[A-Za-z0-9%._~/-]+\?ticket=[A-Za-z0-9._-]+$/

export function parseTicket(value: unknown): MediaTicket {
  if (!isRecord(value) || typeof value.ticket !== 'string' || typeof value.path !== 'string' ||
    !TICKET_PATH.test(value.path) || !safeInteger(value.expiresAtUnixMs) || !safeInteger(value.ttlSeconds)
  ) return invalidPayload('media ticket')
  return { ticket: value.ticket, path: value.path, expiresAtUnixMs: value.expiresAtUnixMs, ttlSeconds: value.ttlSeconds }
}

export async function createTicket(path: string, token: string, ttlSeconds?: number): Promise<MediaTicket> {
  return parseTicket(await request(path, {
    method: 'POST',
    cache: 'no-store',
    headers: mutationHeaders(token),
    body: JSON.stringify(ttlSeconds === undefined ? {} : { ttlSeconds }),
  }))
}

export const createMediaTicket = (
  service: string, application: string, stream: string, token: string, ttlSeconds = MAX_TICKET_TTL_SECONDS,
): Promise<MediaTicket> => createTicket(
  `/api/v1/rtmp/media/${[service, application, stream].map(encodeURIComponent).join('/')}/ticket`, token, ttlSeconds,
)
