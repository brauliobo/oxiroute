import { isRecord } from '../valueGuards'

export class ApiError extends Error {
  readonly status: number
  readonly payload: unknown
  readonly code: string | null

  constructor(status: number, message: string, payload: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.payload = payload
    this.code = apiErrorCode(payload)
  }
}

export async function request(url: string, init?: RequestInit, expectedStatus?: number): Promise<unknown> {
  const response = await fetch(url, init)
  const payload = await response.json() as unknown
  if (!response.ok || (expectedStatus !== undefined && response.status !== expectedStatus)) {
    throw new ApiError(
      response.status,
      apiErrorMessage(payload) ?? `Request returned unexpected status ${response.status}`,
      payload,
    )
  }
  return payload
}

export function apiErrorMessage(value: unknown): string | null {
  return isRecord(value) && isRecord(value.error) && typeof value.error.message === 'string'
    ? value.error.message
    : null
}

function apiErrorCode(value: unknown): string | null {
  return isRecord(value) && isRecord(value.error) && typeof value.error.code === 'string'
    ? value.error.code
    : null
}
