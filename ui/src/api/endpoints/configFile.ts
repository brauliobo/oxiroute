import type {
  CanonicalConfig,
  ConfigDiagnostic,
  ConfigFormat,
  ConfigRequest,
  ConfigSaveResponse,
  ConfigSnapshot,
} from '../../config'
import { isCanonicalConfig } from '../../config'
import { isRecord, safeInteger } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, diagnostics, invalidPayload } from './shared'
import { candidateTopology } from './topology/decode'
import type { CandidateTopologySnapshot } from './topology/types'

export interface ConfigValidationResponse {
  candidateRevision: string
  normalizedConfig: CanonicalConfig
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
  diagnostics: ConfigDiagnostic[]
  restartRequired: boolean
  topology: CandidateTopologySnapshot
}

export async function fetchConfig(token: string, signal?: AbortSignal): Promise<ConfigSnapshot> {
  return parseConfigSnapshot(await request('/api/v1/config', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200))
}

export async function validateConfig(
  config: CanonicalConfig,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigValidationResponse> {
  return parseValidation(await request('/api/v1/config/validate', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ config } satisfies ConfigRequest),
    signal,
  }, 200))
}

export async function saveConfig(
  config: CanonicalConfig,
  diskRevision: string,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigSaveResponse> {
  return parseSave(await request('/api/v1/config', {
    method: 'PUT',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      'If-Config-Revision': diskRevision,
    },
    body: JSON.stringify({ config } satisfies ConfigRequest),
    signal,
  }, 200))
}

function parseConfigSnapshot(value: unknown): ConfigSnapshot {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !isCanonicalConfig(value.config) || !configurationSource(value) || !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration')
  return {
    schemaVersion: 1,
    diskRevision: value.diskRevision,
    candidateRevision: value.candidateRevision,
    activeRevision: value.activeRevision,
    config: value.config,
    configFormat: value.configFormat,
    compositional: value.compositional,
    dependencyCount: value.dependencyCount,
    configPreview: value.configPreview,
    ...(value.luaPreview === undefined ? {} : { luaPreview: value.luaPreview }),
    diagnostics: value.diagnostics,
  }
}

function parseValidation(value: unknown): ConfigValidationResponse {
  if (!isRecord(value) || typeof value.candidateRevision !== 'string' ||
    !isCanonicalConfig(value.normalizedConfig) || !configurationSource(value) ||
    !diagnostics(value.diagnostics) || typeof value.restartRequired !== 'boolean' ||
    !candidateTopology(value.topology)
  ) return invalidPayload('configuration validation')
  return {
    candidateRevision: value.candidateRevision,
    normalizedConfig: value.normalizedConfig,
    configFormat: value.configFormat,
    compositional: value.compositional,
    dependencyCount: value.dependencyCount,
    configPreview: value.configPreview,
    ...(value.luaPreview === undefined ? {} : { luaPreview: value.luaPreview }),
    diagnostics: value.diagnostics,
    restartRequired: value.restartRequired,
    topology: value.topology,
  }
}

function parseSave(value: unknown): ConfigSaveResponse {
  if (!isRecord(value) || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration save')
  const pending = value.outcome === 'saved_pending_activation' &&
    value.activationState === 'pending' && value.restartRequired === false
  const unchanged = value.outcome === 'unchanged_active' &&
    value.activationState === 'active' && value.restartRequired === false
  const restart = value.outcome === 'saved_restart_required' &&
    value.activationState === 'restart_required' && value.restartRequired === true
  if (!pending && !unchanged && !restart) return invalidPayload('configuration save')
  return value as unknown as ConfigSaveResponse
}

function configurationSource(value: Record<string, unknown>): value is Record<string, unknown> & {
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
} {
  return ['kdl', 'lua', 'uci', 'hocon'].includes(String(value.configFormat)) &&
    typeof value.compositional === 'boolean' && safeInteger(value.dependencyCount) &&
    typeof value.configPreview === 'string' &&
    (value.luaPreview === undefined || typeof value.luaPreview === 'string')
}
