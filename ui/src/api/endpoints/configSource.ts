import { isConfigFormat, type ConfigDiagnostic, type ConfigFormat, type ConfigSaveResponse, type StartupOnlyChange } from '../../config'
import { arrayOf, isRecord, nullableString, safeInteger } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, diagnostics, invalidPayload, mutationHeaders, restartReasons } from './shared'

export interface WorkspaceFileEntry {
  path: string
  kind: string
  format: string
  revision: string
  sizeBytes: number
  writable: boolean
}

export interface WorkspaceFileListResponse {
  rootFormat: ConfigFormat
  configWritable: boolean
  writeBlock: string | null
  files: WorkspaceFileEntry[]
}

export interface RedactionInfo {
  policy: string
  redacted: number
  withheld: boolean
}

export interface WorkspaceFileReadResponse {
  path: string
  kind: string
  format: string | null
  fileRevision: string
  isRoot: boolean
  text: string | null
  redaction: RedactionInfo
  configWritable: boolean
  writeBlock: string | null
}

export interface WorkspaceFileValidateResponse {
  valid: boolean
  path: string
  candidateRevision: string
  activeRevision: string | null
  compositional: boolean
  dependencyCount: number
  diagnostics: ConfigDiagnostic[]
  restartRequired: boolean
  restartReasons: StartupOnlyChange[]
  textDiff: string | null
  configWritable: boolean
  writeBlock: string | null
}

export interface ConfigConvertResponse {
  format: ConfigFormat
  text: string | null
  redaction: RedactionInfo
}

export interface ConfigHistoryEntry {
  id: string
  at: number
  bytes: number
  current: boolean
}

export interface ConfigHistoryListResponse {
  diskRevision: string
  activeRevision: string | null
  format: ConfigFormat
  current: { id: string; bytes: number }
  limits: { maxEntries: number; maxTotalBytes: number }
  entries: ConfigHistoryEntry[]
}

export interface ConfigHistoryDiffResponse {
  identical: boolean
  textDiff: string | null
  unresolved: boolean
}

function encodeRelPath(relPath: string): string {
  return relPath.split('/').map(encodeURIComponent).join('/')
}

export async function fetchConfigFiles(token: string, signal?: AbortSignal): Promise<WorkspaceFileListResponse> {
  const value = await request('/api/v1/config/files', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || !isConfigFormat(value.rootFormat) || typeof value.configWritable !== 'boolean' ||
    !nullableString(value.writeBlock) || !arrayOf(value.files, isWorkspaceFileEntry)
  ) return invalidPayload('configuration files list')
  return {
    rootFormat: value.rootFormat,
    configWritable: value.configWritable,
    writeBlock: value.writeBlock,
    files: value.files,
  }
}

export async function fetchConfigFile(relPath: string, token: string, signal?: AbortSignal): Promise<WorkspaceFileReadResponse> {
  const value = await request(`/api/v1/config/files/${encodeRelPath(relPath)}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.path !== 'string' || typeof value.kind !== 'string' ||
    !nullableString(value.format) || typeof value.fileRevision !== 'string' ||
    typeof value.isRoot !== 'boolean' || !nullableString(value.text) ||
    !isRedactionInfo(value.redaction) || typeof value.configWritable !== 'boolean' ||
    !nullableString(value.writeBlock)
  ) return invalidPayload('configuration file')
  return {
    path: value.path,
    kind: value.kind,
    format: value.format,
    fileRevision: value.fileRevision,
    isRoot: value.isRoot,
    text: value.text,
    redaction: value.redaction,
    configWritable: value.configWritable,
    writeBlock: value.writeBlock,
  }
}

export async function validateConfigFile(
  relPath: string,
  text: string,
  token: string,
  signal?: AbortSignal,
): Promise<WorkspaceFileValidateResponse> {
  const value = await request('/api/v1/config/files/validate', {
    method: 'POST',
    headers: mutationHeaders(token),
    body: JSON.stringify({ path: relPath, text, textDiff: true }),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.valid !== 'boolean' || typeof value.path !== 'string' ||
    typeof value.candidateRevision !== 'string' || !nullableString(value.activeRevision) ||
    typeof value.compositional !== 'boolean' || !safeInteger(value.dependencyCount) ||
    !diagnostics(value.diagnostics) || typeof value.restartRequired !== 'boolean' ||
    !restartReasons(value.restartReasons) || !nullableString(value.textDiff) ||
    typeof value.configWritable !== 'boolean' || !nullableString(value.writeBlock)
  ) return invalidPayload('configuration file validation')
  return {
    valid: value.valid,
    path: value.path,
    candidateRevision: value.candidateRevision,
    activeRevision: value.activeRevision,
    compositional: value.compositional,
    dependencyCount: value.dependencyCount,
    diagnostics: value.diagnostics,
    restartRequired: value.restartRequired,
    restartReasons: value.restartReasons,
    textDiff: value.textDiff,
    configWritable: value.configWritable,
    writeBlock: value.writeBlock,
  }
}

export async function saveConfigFile(
  relPath: string,
  text: string,
  fileRevision: string | null,
  create: boolean,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigSaveResponse> {
  const headers: Record<string, string> = mutationHeaders(token)
  if (!create && fileRevision) headers['If-Config-Revision'] = fileRevision
  const value = await request(`/api/v1/config/files/${encodeRelPath(relPath)}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ text, create }),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.diskRevision !== 'string') return invalidPayload('configuration file save')
  return value as unknown as ConfigSaveResponse
}

export async function convertConfig(
  targetFormat: ConfigFormat,
  token: string,
  options?: { text?: string; sourceFormat?: ConfigFormat; view?: 'disk' | 'active' },
  signal?: AbortSignal,
): Promise<ConfigConvertResponse> {
  const value = await request('/api/v1/config/convert', {
    method: 'POST',
    headers: mutationHeaders(token),
    body: JSON.stringify({
      targetFormat,
      ...(options?.text === undefined ? {} : { text: options.text }),
      ...(options?.sourceFormat === undefined ? {} : { sourceFormat: options.sourceFormat }),
      ...(options?.view === undefined ? {} : { view: options.view }),
    }),
    signal,
  }, 200)
  if (!isRecord(value) || !isConfigFormat(value.format) || !nullableString(value.text) || !isRedactionInfo(value.redaction)) {
    return invalidPayload('configuration convert')
  }
  return { format: value.format, text: value.text, redaction: value.redaction }
}

export async function fetchConfigHistory(token: string, signal?: AbortSignal): Promise<ConfigHistoryListResponse> {
  const value = await request('/api/v1/config/history', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.diskRevision !== 'string' || !nullableString(value.activeRevision) ||
    !isConfigFormat(value.format) || !isRecord(value.current) || typeof value.current.id !== 'string' ||
    !safeInteger(value.current.bytes) || !isRecord(value.limits) || !safeInteger(value.limits.maxEntries) ||
    !safeInteger(value.limits.maxTotalBytes) || !arrayOf(value.entries, isHistoryEntry)
  ) return invalidPayload('configuration history')
  return {
    diskRevision: value.diskRevision,
    activeRevision: value.activeRevision,
    format: value.format,
    current: { id: value.current.id, bytes: value.current.bytes },
    limits: { maxEntries: value.limits.maxEntries, maxTotalBytes: value.limits.maxTotalBytes },
    entries: value.entries,
  }
}

export async function fetchConfigHistoryDiff(
  from: string,
  to: string,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigHistoryDiffResponse> {
  const query = new URLSearchParams({ from, to }).toString()
  const value = await request(`/api/v1/config/history/diff?${query}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.identical !== 'boolean' || !nullableString(value.textDiff) ||
    typeof value.unresolved !== 'boolean'
  ) return invalidPayload('configuration history diff')
  return {
    identical: value.identical,
    textDiff: value.textDiff,
    unresolved: value.unresolved,
  }
}

export async function rollbackConfigHistory(
  id: string,
  diskRevision: string,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigSaveResponse> {
  const value = await request(`/api/v1/config/history/${encodeURIComponent(id)}/rollback`, {
    method: 'POST',
    headers: {
      ...mutationHeaders(token),
      'If-Config-Revision': diskRevision,
    },
    body: '{}',
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.diskRevision !== 'string') return invalidPayload('configuration history rollback')
  return value as unknown as ConfigSaveResponse
}

function isWorkspaceFileEntry(value: unknown): value is WorkspaceFileEntry {
  return isRecord(value) && typeof value.path === 'string' && typeof value.kind === 'string' &&
    typeof value.format === 'string' && typeof value.revision === 'string' &&
    safeInteger(value.sizeBytes) && typeof value.writable === 'boolean'
}

function isRedactionInfo(value: unknown): value is RedactionInfo {
  return isRecord(value) && typeof value.policy === 'string' && safeInteger(value.redacted) &&
    typeof value.withheld === 'boolean'
}

function isHistoryEntry(value: unknown): value is ConfigHistoryEntry {
  return isRecord(value) && typeof value.id === 'string' && safeInteger(value.at) &&
    safeInteger(value.bytes) && typeof value.current === 'boolean'
}
