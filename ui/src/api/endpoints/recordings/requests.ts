import { request } from '../../transport'
import { createTicket } from '../rtmp/tickets'
import type { MediaTicket } from '../rtmp/tickets'
import { authorizationHeader } from '../shared'
import { parseDeletedRecording, parseRecordingPage } from './decode'
import type { Recording, RecordingFilter, RecordingPage } from './types'

const BASE = '/api/v1/rtmp/recordings'

function query(filter: RecordingFilter): string {
  const params = new URLSearchParams()
  for (const [name, value] of Object.entries(filter)) if (value !== undefined && value !== '') params.set(name, String(value))
  return params.size === 0 ? '' : `?${params}`
}

export async function fetchRecordings(
  token: string, filter: RecordingFilter = {}, signal?: AbortSignal,
): Promise<RecordingPage> {
  return parseRecordingPage(await request(`${BASE}${query(filter)}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function deleteRecording(id: string, token: string): Promise<Recording> {
  return parseDeletedRecording(await request(`${BASE}/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers: authorizationHeader(token),
  }))
}

export const createRecordingTicket = (id: string, token: string): Promise<MediaTicket> =>
  createTicket(`${BASE}/${encodeURIComponent(id)}/ticket`, token)
