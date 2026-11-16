import { ApiError, request } from '../../transport'
import { authorizationHeader } from '../shared'
import { parseEventPage } from './decode'
import type { EventPage } from './types'

const EVENT_PAGE_PATH_V1 = '/api/v1/events'
const EVENT_PAGE_PATH_V2 = '/api/v2/events'

export async function fetchEvents(
  after: number,
  limit: number,
  token: string,
  signal?: AbortSignal,
): Promise<EventPage> {
  const options = {
    cache: 'no-store' as const,
    headers: authorizationHeader(token),
    signal,
  }
  try {
    return parseEventPage(
      await request(`${EVENT_PAGE_PATH_V2}?after=${after}&limit=${limit}`, options),
      true,
    )
  } catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404) throw error
    return parseEventPage(
      await request(`${EVENT_PAGE_PATH_V1}?after=${after}&limit=${limit}`, options),
      false,
    )
  }
}
