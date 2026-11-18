import { arrayOf, isRecord } from '../../../valueGuards'
import { request } from '../../transport'
import { authorizationHeader, invalidPayload } from '../shared'

export type SecretRole = 'publish' | 'play'

export interface SecretFingerprint {
  service: string
  application: string
  role: SecretRole
  fingerprint: string
}

const FINGERPRINT = /^fp-[0-9a-f]{8}$/

const isFingerprint = (value: unknown): value is SecretFingerprint =>
  isRecord(value) && typeof value.service === 'string' && typeof value.application === 'string' &&
  (value.role === 'publish' || value.role === 'play') && typeof value.fingerprint === 'string' &&
  FINGERPRINT.test(value.fingerprint)

export async function fetchSecretFingerprints(token: string, signal?: AbortSignal): Promise<SecretFingerprint[]> {
  const body = await request('/api/v1/rtmp/secrets', { cache: 'no-store', headers: authorizationHeader(token), signal })
  if (!isRecord(body) || !arrayOf(body.secrets, isFingerprint)) return invalidPayload('RTMP key fingerprints')
  return body.secrets.map(({ service, application, role, fingerprint }) => ({ service, application, role, fingerprint }))
}
