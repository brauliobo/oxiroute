export interface CertbotCertificateSnapshot {
  name: string
  activeArchiveRevision: number
  activeContentRevision: string
  expiresAt: string
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface DirectFileCertificateSnapshot {
  name: string
  activeContentRevision: string
  expiresAt: string
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface AcmeManagedCertificateSnapshot {
  name: string
  directoryUrl: string
  diskRevision: string
  activeRevision: string
  expiresAt: string
  notBeforeUnixSeconds: number | null
  notAfterUnixSeconds: number | null
  nextActionUnixSeconds: number | null
  lastOutcome: string | null
  lastErrorCode: string | null
  renewalInformationStatus: string
  dnsProvider: string | null
  dnsProviderDeployment: string | null
  dnsProviderHealth: string | null
  dnsCleanupStatus: string
}

export interface CertbotWatcherSnapshot {
  health: 'healthy' | 'degraded' | 'stopped'
  coalescedEvents: string
  ignoredAccessEvents: string
  backendErrors: string
  watchRecoveries: string
  watchRefreshes: string
  rescans: string
  periodicRescans: string
  reconciliationFailures: string
}

export type DirectFileWatcherSnapshot = CertbotWatcherSnapshot

export interface TlsMaterialStatus {
  activeContentRevision: string
  expiresAt: string
  activeArchiveRevision?: number
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface TlsManagedCertificateStatus {
  certificate: string
  directoryUrl: string
  challenge: 'http01' | 'dns01' | 'tls_alpn01'
  dnsProvider: string | null
  keyType: string
  allowedDnsSuffixes: string[]
  diskRevision: string
  activeRevision: string
  notBeforeUnixSeconds: number | null
  notAfterUnixSeconds: number | null
  nextActionUnixSeconds: number | null
  notAfter: string
  jobStatus: 'queued' | 'running' | 'waiting_for_challenge' | 'finalizing' | 'paused' | 'succeeded' | 'failed' | 'cancelled' | null
  jobId: string | null
  paused: boolean
  retainedRevisions: number
  retentionDays: number
  retryAttempt: number
  lastSuccessUnixSeconds: number | null
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface TlsCertificateInventory {
  name: string
  dnsNames: string[]
  source: 'files' | 'certbot' | 'acme_managed' | 'self_signed_development'
  developmentOnly: boolean
  status: TlsMaterialStatus | TlsManagedCertificateStatus | null
}

export interface TlsInventory {
  certificates: TlsCertificateInventory[]
  watcher: CertbotWatcherSnapshot | null
}

export interface TlsOperationOutcome {
  certificate: string
  outcome: string
  previousArchiveRevision?: string | null
  archiveRevision?: string
  diskRevision?: string
  activeRevision?: string
  jobId?: string | null
}

export interface TlsReconcileResponse {
  outcomes: TlsOperationOutcome[]
}

export interface TlsRenewResponse extends TlsOperationOutcome {
  diskRevision: string
  activeRevision: string
}

export interface TlsActionResponse extends TlsOperationOutcome {
  jobId: string | null
}
