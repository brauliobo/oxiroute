import type { CanonicalConfig, ConfigDiagnostic, ConfigFormat } from '../../config'
import { isCanonicalConfig, isConfigFormat } from '../../config'
import { isRecord, nullableSafeInteger, nullableString, safeInteger } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, diagnostics, invalidPayload } from './shared'

export interface ImportReportRange {
  start: number
  end: number
}

export interface ImportReportSpan {
  sourceId: number
  range: ImportReportRange
}

export interface ImportReportSourceRoot {
  ordinal: number
  path: string | null
  sourceIds: number[]
  outcome: string | null
}

export interface ImportReportSourceReference {
  id: number
  name: string
  path: string | null
  byteLength: number
  fingerprintSha256: string
}

export interface ImportReportDependency {
  sourceId: number
  targetSourceId: number | null
  kind: string
  requestedPath: string | null
  canonicalPath: string | null
  optional: boolean | null
  status: string
  span: ImportReportSpan | null
  failureCode: string | null
  fingerprintSha256: string | null
  truncated: boolean
}

export interface ImportReportSourceGraph {
  roots: ImportReportSourceRoot[]
  sources: ImportReportSourceReference[]
  dependencies: ImportReportDependency[]
  dependenciesComplete: boolean
  snapshotStable: boolean | null
}

export interface ImportReportInactiveSource {
  condition: string
  origin: ImportReportSpan
}

export interface ImportReportSourceMapSegment {
  generated: ImportReportRange
  original: ImportReportRange
}

export interface ImportReportSourceMap {
  sourceId: number
  segments: ImportReportSourceMapSegment[]
}

export interface ImportReportSourceMetadata {
  environmentFingerprintSha256: string | null
  inactiveSources: ImportReportInactiveSource[]
  originalSourceIds: number[]
  sourceMaps: ImportReportSourceMap[]
}

export interface ImportReportCapabilityProfile {
  id: string
  version: number
}

export interface ImportReportSource {
  product: string
  version: string | null
  versionSource: string | null
  capabilityProfile: ImportReportCapabilityProfile
}

export interface ImportReportCandidateDraft {
  version: number
  maxConnections: number | null
  management: boolean
  stats: boolean
  certificates: number
  tlsProfiles: number
  listeners: number
  upstreamPools: number
  httpServices: number
  cacheStores: number
  forwardProxyServices: number
  rtmpServices: number
  l4Services: number
}

export interface ImportReportOrigin {
  role: string | null
  sourceId: number
  range: ImportReportRange | null
  path: string | null
  line: number | null
  includeStack: ImportReportSpan[]
}

export interface ImportReportProvenance {
  path: string
  origins: ImportReportOrigin[]
}

interface ImportReportCandidateEvidence {
  draft: ImportReportCandidateDraft
  provenance: ImportReportProvenance[]
}

export type ImportReportCandidate = ImportReportCandidateEvidence & (
  | { finalized: true; config: CanonicalConfig }
  | { finalized: false; config: null }
)

export interface ImportReportBlocker {
  id: string
  kind: string
  code: string
  message: string
  scope: string | null
  occurrenceIds: string[]
  origins: ImportReportSpan[]
}

export interface ImportReportRequirement {
  kind: string
  directive: string
  values: string[]
  origins: ImportReportOrigin[]
  equivalentRuntimeEndpoint: boolean | null
}

export interface ImportReportRequirements {
  deployment: ImportReportRequirement[]
  activation: ImportReportRequirement[]
}

export interface ImportReportOverlay {
  id: string
  kind: string
  origin: ImportReportOrigin | null
  redactedEvidence: boolean
  values: string[]
  satisfied: boolean
}

export interface ImportReportDiagnostic {
  code: string
  severity: string
  stage: string
  message: string
  primarySpan: ImportReportSpan | null
  includeStack: ImportReportSpan[]
  relatedSpans: Array<{ span: ImportReportSpan; message: string }>
  help: string | null
}

export interface ImportReportEnvelope {
  schemaVersion: 1
  source: ImportReportSource
  sourceGraph: ImportReportSourceGraph
  sourceMetadata: ImportReportSourceMetadata
  candidate: ImportReportCandidate
  blockers: ImportReportBlocker[]
  requirements: ImportReportRequirements
  overlays: ImportReportOverlay[]
  diagnostics: ImportReportDiagnostic[]
  capabilities?: Record<string, unknown>
}

export type ImportReportStatus = 'draft' | 'finalized' | 'blocked'

export interface ImportReportSummary {
  index: number
  product: string
  version: string | null
  versionSource: string | null
  capabilityProfile: ImportReportCapabilityProfile
  status: ImportReportStatus
  rootCount: number
  sourceCount: number
  dependencyCount: number
  blockerCount: number
  diagnosticCount: number
  provenanceCount: number
  requirementCount: number
  overlayCount: number
  previewAvailable: boolean
}

export interface ImportReportPreview {
  format: 'kdl'
  text: string
}

export interface ImportReportResponse {
  schemaVersion: 1
  diskRevision: string
  candidateRevision: string
  activeRevision: string | null
  configFormat: ConfigFormat
  compositional: boolean
  reports: ImportReportSummary[]
  selection: { index: number } | null
  report: ImportReportEnvelope | null
  preview: ImportReportPreview | null
  diagnostics: ConfigDiagnostic[]
}

export async function fetchImportReports(
  token: string,
  index?: number,
  signal?: AbortSignal,
): Promise<ImportReportResponse> {
  const path = index === undefined ? '/api/v1/import-reports' : `/api/v1/import-reports/${index}`
  return parseImportReportResponse(await request(path, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200))
}

const MAX_IMPORT_REPORTS = 64
const MAX_IMPORT_REPORT_ITEMS = 4_096
const MAX_IMPORT_REPORT_TEXT_BYTES = 2 * 1024 * 1024

function parseImportReportResponse(value: unknown): ImportReportResponse {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !isConfigFormat(value.configFormat) || typeof value.compositional !== 'boolean' ||
    !boundedArray(value.reports, isImportReportSummary, MAX_IMPORT_REPORTS) ||
    !(value.selection === null || isImportReportSelection(value.selection)) ||
    !(value.report === null || isImportReportEnvelope(value.report)) ||
    !(value.preview === null || isImportReportPreview(value.preview)) ||
    !diagnostics(value.diagnostics)
  ) return invalidPayload('native import reports')
  if (value.report !== null && value.selection === null) return invalidPayload('native import reports')
  if (value.report !== null && value.preview !== null && value.report.candidate.config === null) {
    return invalidPayload('native import reports')
  }
  return value as unknown as ImportReportResponse
}

function isImportReportSummary(value: unknown): value is ImportReportSummary {
  return isRecord(value) && safeInteger(value.index) && typeof value.product === 'string' &&
    nullableString(value.version) && nullableString(value.versionSource) &&
    isImportReportCapabilityProfile(value.capabilityProfile) &&
    ['draft', 'finalized', 'blocked'].includes(String(value.status)) &&
    ['rootCount', 'sourceCount', 'dependencyCount', 'blockerCount', 'diagnosticCount',
      'provenanceCount', 'requirementCount', 'overlayCount'].every((key) => safeInteger(value[key])) &&
    typeof value.previewAvailable === 'boolean'
}

function isImportReportSelection(value: unknown): value is { index: number } {
  return isRecord(value) && safeInteger(value.index) && value.index < MAX_IMPORT_REPORTS
}

function isImportReportPreview(value: unknown): value is ImportReportPreview {
  return isRecord(value) && value.format === 'kdl' && boundedText(value.text)
}

function isImportReportEnvelope(value: unknown): value is ImportReportEnvelope {
  return isRecord(value) && value.schemaVersion === 1 && isImportReportSource(value.source) &&
    isImportReportSourceGraph(value.sourceGraph) && isImportReportSourceMetadata(value.sourceMetadata) &&
    isImportReportCandidate(value.candidate) && boundedArray(value.blockers, isImportReportBlocker) &&
    isImportReportRequirements(value.requirements) && boundedArray(value.overlays, isImportReportOverlay) &&
    boundedArray(value.diagnostics, isImportReportDiagnostic) &&
    (value.capabilities === undefined || isRecord(value.capabilities))
}

function isImportReportSource(value: unknown): value is ImportReportSource {
  return isRecord(value) && typeof value.product === 'string' && nullableString(value.version) &&
    nullableString(value.versionSource) && isImportReportCapabilityProfile(value.capabilityProfile)
}

function isImportReportCapabilityProfile(value: unknown): value is ImportReportCapabilityProfile {
  return isRecord(value) && typeof value.id === 'string' && safeInteger(value.version)
}

function isImportReportSourceGraph(value: unknown): value is ImportReportSourceGraph {
  return isRecord(value) && boundedArray(value.roots, isImportReportSourceRoot) &&
    boundedArray(value.sources, isImportReportSourceReference) &&
    boundedArray(value.dependencies, isImportReportDependency) &&
    typeof value.dependenciesComplete === 'boolean' &&
    (value.snapshotStable === null || typeof value.snapshotStable === 'boolean')
}

function isImportReportSourceRoot(value: unknown): value is ImportReportSourceRoot {
  return isRecord(value) && safeInteger(value.ordinal) && nullableString(value.path) &&
    boundedArray(value.sourceIds, safeIntegerValue) && nullableString(value.outcome)
}

function isImportReportSourceReference(value: unknown): value is ImportReportSourceReference {
  return isRecord(value) && safeInteger(value.id) && typeof value.name === 'string' &&
    nullableString(value.path) && safeInteger(value.byteLength) && typeof value.fingerprintSha256 === 'string'
}

function isImportReportDependency(value: unknown): value is ImportReportDependency {
  return isRecord(value) && safeInteger(value.sourceId) &&
    (value.targetSourceId === null || safeInteger(value.targetSourceId)) && typeof value.kind === 'string' &&
    nullableString(value.requestedPath) && nullableString(value.canonicalPath) &&
    (value.optional === null || typeof value.optional === 'boolean') && typeof value.status === 'string' &&
    (value.span === null || isImportReportSpan(value.span)) && nullableString(value.failureCode) &&
    nullableString(value.fingerprintSha256) && typeof value.truncated === 'boolean'
}

function isImportReportSourceMetadata(value: unknown): value is ImportReportSourceMetadata {
  return isRecord(value) && nullableString(value.environmentFingerprintSha256) &&
    boundedArray(value.inactiveSources, isImportReportInactiveSource) &&
    boundedArray(value.originalSourceIds, safeIntegerValue) &&
    boundedArray(value.sourceMaps, isImportReportSourceMap)
}

function isImportReportInactiveSource(value: unknown): value is ImportReportInactiveSource {
  return isRecord(value) && typeof value.condition === 'string' && isImportReportSpan(value.origin)
}

function isImportReportSourceMap(value: unknown): value is ImportReportSourceMap {
  return isRecord(value) && safeInteger(value.sourceId) &&
    boundedArray(value.segments, isImportReportSourceMapSegment)
}

function isImportReportSourceMapSegment(value: unknown): value is ImportReportSourceMapSegment {
  return isRecord(value) && isImportReportRange(value.generated) && isImportReportRange(value.original)
}

function isImportReportRange(value: unknown): value is ImportReportRange {
  return isRecord(value) && safeInteger(value.start) && safeInteger(value.end) && value.end >= value.start
}

function isImportReportSpan(value: unknown): value is ImportReportSpan {
  return isRecord(value) && safeInteger(value.sourceId) && isImportReportRange(value.range)
}

function isImportReportCandidate(value: unknown): value is ImportReportCandidate {
  return isRecord(value) &&
    ((value.finalized === true && isCanonicalConfig(value.config)) ||
      (value.finalized === false && value.config === null)) &&
    isImportReportCandidateDraft(value.draft) && boundedArray(value.provenance, isImportReportProvenance)
}

function isImportReportCandidateDraft(value: unknown): value is ImportReportCandidateDraft {
  return isRecord(value) && safeInteger(value.version) && nullableSafeInteger(value.maxConnections) &&
    typeof value.management === 'boolean' && typeof value.stats === 'boolean' &&
    ['certificates', 'tlsProfiles', 'listeners', 'upstreamPools', 'httpServices', 'cacheStores',
      'forwardProxyServices', 'rtmpServices', 'l4Services'].every((key) => safeInteger(value[key]))
}

function isImportReportProvenance(value: unknown): value is ImportReportProvenance {
  return isRecord(value) && typeof value.path === 'string' && boundedArray(value.origins, isImportReportOrigin)
}

function isImportReportOrigin(value: unknown): value is ImportReportOrigin {
  return isRecord(value) && nullableString(value.role) && safeInteger(value.sourceId) &&
    (value.range === null || isImportReportRange(value.range)) && nullableString(value.path) &&
    nullableSafeInteger(value.line) && boundedArray(value.includeStack, isImportReportSpan)
}

function isImportReportBlocker(value: unknown): value is ImportReportBlocker {
  return isRecord(value) && typeof value.id === 'string' && typeof value.kind === 'string' &&
    typeof value.code === 'string' && typeof value.message === 'string' && nullableString(value.scope) &&
    boundedArray(value.occurrenceIds, (entry): entry is string => typeof entry === 'string') &&
    boundedArray(value.origins, isImportReportSpan)
}

function isImportReportRequirements(value: unknown): value is ImportReportRequirements {
  return isRecord(value) && boundedArray(value.deployment, isImportReportRequirement) &&
    boundedArray(value.activation, isImportReportRequirement)
}

function isImportReportRequirement(value: unknown): value is ImportReportRequirement {
  return isRecord(value) && typeof value.kind === 'string' && typeof value.directive === 'string' &&
    boundedArray(value.values, (entry): entry is string => typeof entry === 'string') &&
    boundedArray(value.origins, isImportReportOrigin) &&
    (value.equivalentRuntimeEndpoint === null || typeof value.equivalentRuntimeEndpoint === 'boolean')
}

function isImportReportOverlay(value: unknown): value is ImportReportOverlay {
  return isRecord(value) && typeof value.id === 'string' && typeof value.kind === 'string' &&
    (value.origin === null || isImportReportOrigin(value.origin)) && typeof value.redactedEvidence === 'boolean' &&
    boundedArray(value.values, (entry): entry is string => typeof entry === 'string') &&
    typeof value.satisfied === 'boolean'
}

function isImportReportDiagnostic(value: unknown): value is ImportReportDiagnostic {
  return isRecord(value) && typeof value.code === 'string' && typeof value.severity === 'string' &&
    typeof value.stage === 'string' && typeof value.message === 'string' &&
    (value.primarySpan === null || isImportReportSpan(value.primarySpan)) &&
    boundedArray(value.includeStack, isImportReportSpan) && boundedArray(value.relatedSpans, isRelatedImportSpan) &&
    nullableString(value.help)
}

function isRelatedImportSpan(value: unknown): value is { span: ImportReportSpan; message: string } {
  return isRecord(value) && isImportReportSpan(value.span) && typeof value.message === 'string'
}

function boundedArray<T>(value: unknown, predicate: (entry: unknown) => entry is T, limit = MAX_IMPORT_REPORT_ITEMS): value is T[] {
  return Array.isArray(value) && value.length <= limit && value.every(predicate)
}

function safeIntegerValue(value: unknown): value is number {
  return safeInteger(value)
}

function boundedText(value: unknown): value is string {
  return typeof value === 'string' && value.length <= MAX_IMPORT_REPORT_TEXT_BYTES
}
