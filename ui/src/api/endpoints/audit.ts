import { isRecord, safeInteger } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, invalidPayload } from './shared'

export type AuditCategory = 'reload' | 'import' | 'certificate' | 'control'
export type AuditResult = 'requested' | 'succeeded' | 'failed' | 'rejected' | 'conflict' | 'partial' | 'degraded'

export interface AuditRecord {
  id: number
  timestampUnixMs: number
  correlationId: string
  actor: string
  source: string
  category: AuditCategory
  operation: string
  result: AuditResult
  revision?: string
}

export interface AuditPage {
  records: AuditRecord[]
  cursor: number
  latestCursor: number
  hasMore: boolean
  oldestCursor: number | null
}

export type AuditComponentState = 'healthy' | 'degraded' | 'memory'

export interface AuditStatus {
  state: AuditComponentState
  persistent: boolean
  degraded: boolean
  recordCount: number
  bytes: number
  rotatedFiles: number
  maxRecords: number
  maxRecordBytes: number
  maxFileBytes: number
  maxTotalBytes: number
  maxRotatedFiles: number
  writeFailures: number
  corruptRecords: number
  lastError?: string
}

export interface AuditStatusResponse {
  audit: AuditStatus
}

export interface AuditQuery {
  after: number
  limit: number
  category?: AuditCategory
  result?: AuditResult
}

export async function fetchAudit(
  query: AuditQuery,
  token: string,
  signal?: AbortSignal,
): Promise<AuditPage> {
  const params = new URLSearchParams({ after: String(query.after), limit: String(query.limit) })
  if (query.category !== undefined) params.set('category', query.category)
  if (query.result !== undefined) params.set('result', query.result)
  return parseAuditPage(await request(`/api/v1/audit?${params.toString()}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchAuditStatus(token: string, signal?: AbortSignal): Promise<AuditStatusResponse> {
  return parseAuditStatus(await request('/api/v1/audit/status', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

function parseAuditPage(value: unknown): AuditPage {
  if (!isRecord(value) || !Array.isArray(value.records) ||
    !value.records.every((record) => auditRecord(record) !== null) ||
    !safeInteger(value.cursor) || !safeInteger(value.latestCursor) ||
    typeof value.hasMore !== 'boolean' ||
    !(value.oldestCursor === null || safeInteger(value.oldestCursor))
  ) return invalidPayload('audit history')
  const records = value.records.map((record) => parseAuditRecord(record as Record<string, unknown>))
  if (records.some((record): record is null => record === null)) return invalidPayload('audit history')
  return {
    records: records as AuditRecord[],
    cursor: value.cursor,
    latestCursor: value.latestCursor,
    hasMore: value.hasMore,
    oldestCursor: value.oldestCursor,
  }
}

function parseAuditStatus(value: unknown): AuditStatusResponse {
  if (!isRecord(value) || !auditStatus(value.audit)) return invalidPayload('audit status')
  const status = value.audit as Record<string, unknown>
  return {
    audit: {
      state: status.state as AuditComponentState,
      persistent: status.persistent as boolean,
      degraded: status.degraded as boolean,
      recordCount: status.recordCount as number,
      bytes: status.bytes as number,
      rotatedFiles: status.rotatedFiles as number,
      maxRecords: status.maxRecords as number,
      maxRecordBytes: status.maxRecordBytes as number,
      maxFileBytes: status.maxFileBytes as number,
      maxTotalBytes: status.maxTotalBytes as number,
      maxRotatedFiles: status.maxRotatedFiles as number,
      writeFailures: status.writeFailures as number,
      corruptRecords: status.corruptRecords as number,
      ...(status.lastError === undefined ? {} : { lastError: status.lastError as string }),
    },
  }
}

function parseAuditRecord(value: Record<string, unknown>): AuditRecord | null {
  if (!auditRecord(value)) return null
  return {
    id: value.id as number,
    timestampUnixMs: value.timestampUnixMs as number,
    correlationId: value.correlationId as string,
    actor: value.actor as string,
    source: value.source as string,
    category: value.category as AuditCategory,
    operation: value.operation as string,
    result: value.result as AuditResult,
    ...(value.revision === undefined ? {} : { revision: value.revision as string }),
  }
}

function auditRecord(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && safeInteger(value.id) && safeInteger(value.timestampUnixMs) &&
    typeof value.correlationId === 'string' && typeof value.actor === 'string' &&
    typeof value.source === 'string' && isAuditCategory(value.category) &&
    typeof value.operation === 'string' && isAuditResult(value.result) &&
    (value.revision === undefined || typeof value.revision === 'string')
}

function auditStatus(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && ['healthy', 'degraded', 'memory'].includes(String(value.state)) &&
    typeof value.persistent === 'boolean' && typeof value.degraded === 'boolean' &&
    safeInteger(value.recordCount) && safeInteger(value.bytes) && safeInteger(value.rotatedFiles) &&
    safeInteger(value.maxRecords) && safeInteger(value.maxRecordBytes) &&
    safeInteger(value.maxFileBytes) && safeInteger(value.maxTotalBytes) &&
    safeInteger(value.maxRotatedFiles) && safeInteger(value.writeFailures) &&
    safeInteger(value.corruptRecords) &&
    (value.lastError === undefined || typeof value.lastError === 'string')
}

function isAuditCategory(value: unknown): value is AuditCategory {
  return ['reload', 'import', 'certificate', 'control'].includes(String(value))
}

function isAuditResult(value: unknown): value is AuditResult {
  return ['requested', 'succeeded', 'failed', 'rejected', 'conflict', 'partial', 'degraded']
    .includes(String(value))
}
