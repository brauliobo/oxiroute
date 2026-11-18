import { request } from '../../transport'
import { fetchGenerations } from '../runtime/requests'
import { authorizationHeader } from '../shared'
import { parseRecorder, parseRtmpCatalog, parseRtmpControlResponse, parseRtmpStats } from './decode'
import type {
  RecorderSnapshot,
  RtmpCatalog,
  RtmpClientControlTarget,
  RtmpClientSnapshot,
  RtmpControlResponse,
  RtmpStats,
} from './types'

export async function fetchRtmpCatalog(token: string, signal?: AbortSignal): Promise<RtmpCatalog> {
  return parseRtmpCatalog(await request('/api/v1/rtmp/streams', {
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchRtmpStats(token: string, signal?: AbortSignal): Promise<RtmpStats> {
  return parseRtmpStats(await request('/api/v1/rtmp/stats', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function setRecording(
  streamId: string,
  recorderId: string,
  action: 'start' | 'stop',
  token: string,
): Promise<RecorderSnapshot> {
  const headers = {
    ...authorizationHeader(token),
    'If-Generation-Revision': await fetchActiveRevision(token),
  }
  return parseRecorder(await request(
    `/api/v1/rtmp/streams/${streamId}/recorders/${recorderId}/${action}`,
    { method: 'POST', headers },
  ))
}

export async function dropRtmpClient(
  client: RtmpClientSnapshot,
  target: RtmpClientControlTarget,
  token: string,
): Promise<RtmpControlResponse> {
  const suffix = target === 'client' ? 'drop' : `${target}/drop`
  return parseRtmpControlResponse(await request(
    `/api/v1/rtmp/clients/${client.id}/${suffix}`,
    {
      method: 'POST',
      headers: {
        ...authorizationHeader(token),
        'If-Rtmp-Session-Revision': client.revision,
      },
    },
    202,
  ))
}

async function fetchActiveRevision(token: string): Promise<string> {
  const value = await fetchGenerations(token)
  if (value.generation.activeRevision === null) {
    throw new Error('generation response has no active revision')
  }
  return value.generation.activeRevision
}
