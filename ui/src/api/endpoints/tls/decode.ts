import { decimalString, isRecord, nullableSafeInteger, nullableString, safeInteger } from '../../../valueGuards'
import { invalidPayload } from '../shared'
import type {
  AcmeManagedCertificateSnapshot,
  CertbotCertificateSnapshot,
  CertbotWatcherSnapshot,
  DirectFileCertificateSnapshot,
  TlsActionResponse,
  TlsCertificateInventory,
  TlsInventory,
  TlsManagedCertificateStatus,
  TlsMaterialStatus,
  TlsOperationOutcome,
  TlsReconcileResponse,
  TlsRenewResponse,
} from './types'

export function parseTlsInventory(value: unknown): TlsInventory {
  if (!isRecord(value) || !Array.isArray(value.certificates) ||
    !value.certificates.every(tlsCertificateInventory) ||
    !(value.watcher === null || certbotWatcher(value.watcher))
  ) return invalidPayload('TLS inventory')
  return {
    certificates: value.certificates.map((certificate) => normalizeTlsCertificate(certificate as Record<string, unknown>)),
    watcher: value.watcher as CertbotWatcherSnapshot | null,
  }
}

export function parseTlsReconcileResponse(value: unknown): TlsReconcileResponse {
  if (!isRecord(value) || !Array.isArray(value.outcomes) || !value.outcomes.every(tlsOperationOutcome)) {
    return invalidPayload('TLS reconciliation')
  }
  return { outcomes: value.outcomes.map((outcome) => normalizeTlsOperationOutcome(outcome as Record<string, unknown>)) }
}

export function parseTlsRenewResponse(value: unknown): TlsRenewResponse {
  if (!tlsOperationOutcome(value) || typeof value.diskRevision !== 'string' ||
    typeof value.activeRevision !== 'string'
  ) return invalidPayload('TLS renewal')
  return {
    ...normalizeTlsOperationOutcome(value),
    diskRevision: value.diskRevision,
    activeRevision: value.activeRevision,
  }
}

export function parseTlsActionResponse(value: unknown): TlsActionResponse {
  if (!tlsOperationOutcome(value)) return invalidPayload('TLS action')
  return {
    ...normalizeTlsOperationOutcome(value),
    jobId: value.jobId as string | null | undefined ?? null,
  }
}

function tlsCertificateInventory(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value) || typeof value.name !== 'string' || !Array.isArray(value.dnsNames) ||
    !value.dnsNames.every((name) => typeof name === 'string') ||
    !['files', 'certbot', 'acme_managed', 'self_signed_development'].includes(String(value.source)) ||
    typeof value.developmentOnly !== 'boolean'
  ) return false
  if (value.status === null) return true
  if (!isRecord(value.status)) return false
  return value.source === 'acme_managed' ? tlsManagedCertificateStatus(value.status) : tlsMaterialStatus(value.status)
}

function tlsMaterialStatus(value: unknown): value is Record<string, unknown> & TlsMaterialStatus {
  return isRecord(value) && typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    (value.activeArchiveRevision === undefined || safeInteger(value.activeArchiveRevision)) &&
    (value.lastOutcome === undefined || nullableString(value.lastOutcome)) &&
    (value.lastErrorCode === undefined || nullableString(value.lastErrorCode))
}

function tlsManagedCertificateStatus(value: unknown): value is Record<string, unknown> & TlsManagedCertificateStatus {
  return isRecord(value) && typeof value.certificate === 'string' && typeof value.directoryUrl === 'string' &&
    ['http01', 'dns01', 'tls_alpn01'].includes(String(value.challenge)) && nullableString(value.dnsProvider) &&
    typeof value.keyType === 'string' && Array.isArray(value.allowedDnsSuffixes) && value.allowedDnsSuffixes.every((suffix) => typeof suffix === 'string') &&
    typeof value.diskRevision === 'string' && typeof value.activeRevision === 'string' &&
    nullableSafeInteger(value.notBeforeUnixSeconds) && nullableSafeInteger(value.notAfterUnixSeconds) &&
    nullableSafeInteger(value.nextActionUnixSeconds) && typeof value.notAfter === 'string' &&
     (value.jobStatus === null || ['queued', 'running', 'waiting_for_challenge', 'finalizing', 'paused', 'succeeded', 'failed', 'cancelled'].includes(String(value.jobStatus))) &&
     nullableString(value.jobId) && typeof value.paused === 'boolean' &&
     safeInteger(value.retainedRevisions) && safeInteger(value.retentionDays) &&
    safeInteger(value.retryAttempt) && nullableSafeInteger(value.lastSuccessUnixSeconds) &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function normalizeTlsCertificate(value: Record<string, unknown>): TlsCertificateInventory {
  const status = value.status === null
    ? null
    : value.source === 'acme_managed'
      ? normalizeTlsManagedStatus(value.status as Record<string, unknown>)
      : normalizeTlsMaterialStatus(value.status as Record<string, unknown>)
  return {
    name: value.name as string,
    dnsNames: value.dnsNames as string[],
    source: value.source as TlsCertificateInventory['source'],
    developmentOnly: value.developmentOnly as boolean,
    status,
  }
}

function normalizeTlsMaterialStatus(value: Record<string, unknown>): TlsMaterialStatus {
  return {
    activeContentRevision: value.activeContentRevision as string,
    expiresAt: value.expiresAt as string,
    ...(value.activeArchiveRevision === undefined ? {} : { activeArchiveRevision: value.activeArchiveRevision as number }),
    lastOutcome: value.lastOutcome as string | null | undefined ?? null,
    lastErrorCode: value.lastErrorCode as string | null | undefined ?? null,
  }
}

function normalizeTlsManagedStatus(value: Record<string, unknown>): TlsManagedCertificateStatus {
  return {
    certificate: value.certificate as string,
    directoryUrl: value.directoryUrl as string,
    challenge: value.challenge as TlsManagedCertificateStatus['challenge'],
    dnsProvider: value.dnsProvider as string | null,
    keyType: value.keyType as string,
    allowedDnsSuffixes: value.allowedDnsSuffixes as string[],
    diskRevision: value.diskRevision as string,
    activeRevision: value.activeRevision as string,
    notBeforeUnixSeconds: value.notBeforeUnixSeconds as number | null,
    notAfterUnixSeconds: value.notAfterUnixSeconds as number | null,
    nextActionUnixSeconds: value.nextActionUnixSeconds as number | null,
    notAfter: value.notAfter as string,
    jobStatus: value.jobStatus as TlsManagedCertificateStatus['jobStatus'],
    jobId: value.jobId as string | null,
    paused: value.paused as boolean,
    retainedRevisions: value.retainedRevisions as number,
    retentionDays: value.retentionDays as number,
    retryAttempt: value.retryAttempt as number,
    lastSuccessUnixSeconds: value.lastSuccessUnixSeconds as number | null,
    lastOutcome: value.lastOutcome as string | null,
    lastErrorCode: value.lastErrorCode as string | null,
  }
}

function tlsOperationOutcome(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value) || typeof value.certificate !== 'string' || typeof value.outcome !== 'string') return false
  return optionalStringOrNull(value.previousArchiveRevision) && optionalString(value.archiveRevision) &&
    optionalString(value.diskRevision) && optionalString(value.activeRevision) &&
    optionalStringOrNull(value.jobId)
}

function normalizeTlsOperationOutcome(value: Record<string, unknown>): TlsOperationOutcome {
  return {
    certificate: value.certificate as string,
    outcome: value.outcome as string,
    ...(value.previousArchiveRevision === undefined ? {} : { previousArchiveRevision: value.previousArchiveRevision as string | null }),
    ...(value.archiveRevision === undefined ? {} : { archiveRevision: value.archiveRevision as string }),
    ...(value.diskRevision === undefined ? {} : { diskRevision: value.diskRevision as string }),
    ...(value.activeRevision === undefined ? {} : { activeRevision: value.activeRevision as string }),
    jobId: value.jobId as string | null | undefined ?? null,
  }
}

function optionalString(value: unknown): boolean {
  return value === undefined || typeof value === 'string'
}

function optionalStringOrNull(value: unknown): boolean {
  return value === undefined || value === null || typeof value === 'string'
}

export function projectCertbotCertificate(value: CertbotCertificateSnapshot): CertbotCertificateSnapshot {
  return {
    name: value.name,
    activeArchiveRevision: value.activeArchiveRevision,
    activeContentRevision: value.activeContentRevision,
    expiresAt: value.expiresAt,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
  }
}

export function projectAcmeManagedCertificate(
  value: AcmeManagedCertificateSnapshot,
): AcmeManagedCertificateSnapshot {
  return {
    name: value.name,
    directoryUrl: value.directoryUrl,
    diskRevision: value.diskRevision,
    activeRevision: value.activeRevision,
    expiresAt: value.expiresAt,
    notBeforeUnixSeconds: value.notBeforeUnixSeconds,
    notAfterUnixSeconds: value.notAfterUnixSeconds,
    nextActionUnixSeconds: value.nextActionUnixSeconds,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
    renewalInformationStatus: value.renewalInformationStatus,
    dnsProvider: value.dnsProvider,
    dnsProviderDeployment: value.dnsProviderDeployment,
    dnsProviderHealth: value.dnsProviderHealth,
    dnsCleanupStatus: value.dnsCleanupStatus,
  }
}

export function projectDirectFileCertificate(
  value: DirectFileCertificateSnapshot,
): DirectFileCertificateSnapshot {
  return {
    name: value.name,
    activeContentRevision: value.activeContentRevision,
    expiresAt: value.expiresAt,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
  }
}

export function projectCertbotWatcher(value: CertbotWatcherSnapshot): CertbotWatcherSnapshot {
  return {
    health: value.health,
    coalescedEvents: value.coalescedEvents,
    ignoredAccessEvents: value.ignoredAccessEvents,
    backendErrors: value.backendErrors,
    watchRecoveries: value.watchRecoveries,
    watchRefreshes: value.watchRefreshes,
    rescans: value.rescans,
    periodicRescans: value.periodicRescans,
    reconciliationFailures: value.reconciliationFailures,
  }
}

export function certbotCertificate(value: unknown): value is CertbotCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' && safeInteger(value.activeArchiveRevision) &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

export function acmeManagedCertificate(value: unknown): value is AcmeManagedCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' && typeof value.directoryUrl === 'string' &&
    typeof value.diskRevision === 'string' && typeof value.activeRevision === 'string' &&
    typeof value.expiresAt === 'string' && nullableSafeInteger(value.notBeforeUnixSeconds) &&
    nullableSafeInteger(value.notAfterUnixSeconds) && nullableSafeInteger(value.nextActionUnixSeconds) &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode) &&
    typeof value.renewalInformationStatus === 'string' && nullableString(value.dnsProvider) &&
    nullableString(value.dnsProviderDeployment) && nullableString(value.dnsProviderHealth) &&
    typeof value.dnsCleanupStatus === 'string'
}

export function directFileCertificate(value: unknown): value is DirectFileCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

export function certbotWatcher(value: unknown): value is CertbotWatcherSnapshot {
  return isRecord(value) && typeof value.health === 'string' &&
    ['healthy', 'degraded', 'stopped'].includes(value.health) &&
    ['coalescedEvents', 'ignoredAccessEvents', 'backendErrors', 'watchRecoveries', 'watchRefreshes',
      'rescans', 'periodicRescans', 'reconciliationFailures'].every((key) => decimalString(value[key]))
}
