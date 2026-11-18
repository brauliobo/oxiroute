import type { ButtonVariant } from '#design/button/types'
import {
  cancelManagedJob,
  deleteManagedCertificate,
  pauseManagedJobs,
  reconcileTls,
  renewManagedCertificate,
  resumeManagedJobs,
  revokeManagedCertificate,
  rolloverManagedAccountKey,
} from '../api'

export interface CertificateActionSpec {
  label: string
  variant: ButtonVariant
  phrase: (certificate: string) => string
  run: (revision: string, token: string, certificate: string) => Promise<unknown>
}

export const CERTIFICATE_ACTIONS = {
  reconcile: {
    label: 'Reconcile status',
    variant: 'default',
    phrase: (name) => `reconcile ${name}`,
    run: (revision, token, name) => reconcileTls(revision, token, name),
  },
  renew: {
    label: 'Renew now',
    variant: 'primary',
    phrase: (name) => `renew ${name}`,
    run: (revision, token, name) => renewManagedCertificate(revision, token, name),
  },
  rollover: {
    label: 'Rollover account key',
    variant: 'default',
    phrase: (name) => `roll over the account key for ${name}`,
    run: (revision, token, name) => rolloverManagedAccountKey(revision, token, name),
  },
  revoke: {
    label: 'Revoke certificate',
    variant: 'danger',
    phrase: (name) => `revoke ${name} at the ACME CA`,
    run: (revision, token, name) => revokeManagedCertificate(revision, token, name),
  },
  removeState: {
    label: 'Delete stored state',
    variant: 'danger',
    phrase: (name) => `delete stored state for ${name}`,
    run: (revision, token, name) => deleteManagedCertificate(revision, token, name),
  },
  pause: {
    label: 'Pause renewal',
    variant: 'default',
    phrase: (name) => `pause renewal for ${name}`,
    run: (revision, token, name) => pauseManagedJobs(revision, token, name),
  },
  resume: {
    label: 'Resume renewal',
    variant: 'default',
    phrase: (name) => `resume renewal for ${name}`,
    run: (revision, token, name) => resumeManagedJobs(revision, token, name),
  },
  cancel: {
    label: 'Cancel active job',
    variant: 'danger',
    phrase: (name) => `cancel the active job for ${name}`,
    run: (revision, token, name) => cancelManagedJob(revision, token, name),
  },
} as const satisfies Record<string, CertificateActionSpec>

export type CertificateAction = keyof typeof CERTIFICATE_ACTIONS
