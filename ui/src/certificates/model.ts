import type { Tone } from '#design/badge/tone'
import { formatTimestamp } from '#design/format'
import type { TlsCertificateInventory, TlsManagedCertificateStatus, TlsMaterialStatus } from '../api'
import { shortRevision } from '../formatters'
import { plural } from '../workspace/plural'
import type { Fact } from '../workspace/types'
import type { CertificateAction } from './actions'
import { expiryPhrase } from './expiry'

type Status = TlsMaterialStatus | TlsManagedCertificateStatus

const SOURCE_LABELS: Readonly<Record<TlsCertificateInventory['source'], string>> = {
  files: 'Files',
  certbot: 'Certbot',
  acme_managed: 'ACME managed',
  self_signed_development: 'Self-signed, development',
}

export const isManaged = (status: Status | null): status is TlsManagedCertificateStatus =>
  status !== null && 'directoryUrl' in status

export const sourceLabel = (certificate: TlsCertificateInventory): string => SOURCE_LABELS[certificate.source]

export const expiresAtMs = (status: Status): number => Date.parse(isManaged(status) ? status.notAfter : status.expiresAt)

export function statusBadge(certificate: TlsCertificateInventory): { tone: Tone; label: string } {
  const { status } = certificate
  if (!status) return { tone: 'bad', label: 'No active material' }
  if (status.lastErrorCode) return { tone: 'bad', label: 'Error reported' }
  if (isManaged(status) && status.paused) return { tone: 'warn', label: 'Renewal paused' }
  return certificate.developmentOnly ? { tone: 'info', label: 'Development only' } : { tone: 'ok', label: 'Loaded' }
}

export function certificateActions(certificate: TlsCertificateInventory): CertificateAction[] {
  const { source, status } = certificate
  const managed = isManaged(status)
  const acme = source === 'acme_managed'
  return [
    (acme || source === 'certbot') && 'reconcile',
    acme && 'renew',
    acme && 'rollover',
    acme && 'revoke',
    acme && 'removeState',
    managed && (status.paused ? 'resume' : 'pause'),
    managed && status.jobId && 'cancel',
  ].filter((action): action is CertificateAction => Boolean(action))
}

const listOr = (values: readonly string[], empty: string) => (values.length ? values.join(', ') : empty)

function managedFacts(status: TlsManagedCertificateStatus): Fact[] {
  const next = status.nextActionUnixSeconds === null ? 'Not scheduled' : formatTimestamp(status.nextActionUnixSeconds * 1000)
  return [
    { label: 'Directory', value: status.directoryUrl, code: true },
    { label: 'Challenge', value: status.challenge },
    ...(status.dnsProvider ? [{ label: 'DNS provider', value: status.dnsProvider }] : []),
    { label: 'Key policy', value: status.keyType },
    { label: 'Allowed suffixes', value: listOr(status.allowedDnsSuffixes, 'None') },
    { label: 'Next action', value: next },
    { label: 'Retention', value: `${plural(status.retainedRevisions, 'revision')}, ${plural(status.retentionDays, 'day')}` },
    ...(status.jobId ? [{ label: 'Active job', value: status.jobId, code: true }] : []),
  ]
}

function materialFacts(status: Status, nowMs: number): Fact[] {
  const expires = expiresAtMs(status)
  const content = isManaged(status) ? status.activeRevision : status.activeContentRevision
  const archive = isManaged(status) ? undefined : status.activeArchiveRevision
  return [
    { label: 'Expires', value: `${formatTimestamp(expires)}, ${expiryPhrase(expires, nowMs)}` },
    { label: 'Active content', value: shortRevision(content), code: true },
    ...(archive === undefined ? [] : [{ label: 'Active archive', value: String(archive) }]),
  ]
}

export function certificateFacts(certificate: TlsCertificateInventory, nowMs: number): Fact[] {
  const { status } = certificate
  return [
    { label: 'DNS names', value: listOr(certificate.dnsNames, 'None reported') },
    ...(status === null ? [] : materialFacts(status, nowMs)),
    ...(isManaged(status) ? managedFacts(status) : []),
    ...(status?.lastErrorCode ? [{ label: 'Last error code', value: status.lastErrorCode, code: true }] : []),
  ]
}
