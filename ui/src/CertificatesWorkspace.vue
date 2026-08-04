<template lang="pug">
section.certificates-workspace(aria-labelledby="certificates-heading" :aria-busy="loading || mutating")
  header.workspace-heading
    div
      p.eyebrow Certificate operations
      h2#certificates-heading Certificates
      p.workspace-deck Inventory is intentionally redacted. Renewal and reconciliation use the current active-generation revision and require an explicit confirmation.
    button.secondary-button(type="button" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh certificates' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to inspect certificate status and jobs.

  .loading-panel(v-else-if="loading && !inventory" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading certificate inventory
      p Waiting for the redacted TLS status contract.

  p.notice.error-notice(v-if="error" role="alert")
    strong Certificate operation unavailable.
    |  {{ error }}
  p.notice.success-notice(v-if="message" role="status" aria-live="polite") {{ message }}

  template(v-if="token && inventory")
    section.watcher-panel(aria-labelledby="watcher-heading" v-if="inventory.watcher")
      .section-heading
        div
          p.eyebrow Certbot lineage watcher
          h3#watcher-heading Watcher health
        span.status-chip(:class="`watcher-${inventory.watcher.health}`") {{ inventory.watcher.health }}
      .watcher-grid
        .watcher-metric
          span.label Backend errors
          strong {{ formatCount(inventory.watcher.backendErrors) }}
        .watcher-metric
          span.label Reconciliations failed
          strong {{ formatCount(inventory.watcher.reconciliationFailures) }}
        .watcher-metric
          span.label Refreshes
          strong {{ formatCount(inventory.watcher.watchRefreshes) }}
        .watcher-metric
          span.label Rescans
          strong {{ formatCount(inventory.watcher.rescans) }}

    section.inventory-panel(aria-labelledby="inventory-heading")
      header.panel-heading
        div
          p.eyebrow Redacted inventory
          h3#inventory-heading Configured certificates
        span.panel-note {{ inventory.certificates.length }}
      p.empty-list(v-if="inventory.certificates.length === 0") No certificates are configured.
      .certificate-grid(v-else)
        article.certificate-card(v-for="certificate in inventory.certificates" :key="certificate.name")
          header.certificate-heading
            div
              span.source-chip {{ certificate.source.replaceAll('_', ' ') }}
              h4 {{ certificate.name }}
            span.status-chip(:class="certificateStatusClass(certificate)") {{ certificateStatusLabel(certificate) }}
          .certificate-facts
            .fact
              span.label DNS names
              strong {{ certificate.dnsNames.length ? certificate.dnsNames.join(', ') : 'None reported' }}
            .fact(v-if="isManaged(certificate.status)")
              span.label Directory
              code {{ certificate.status.directoryUrl }}
            .fact(v-if="isManaged(certificate.status)")
              span.label Key policy
              strong {{ certificate.status.keyType }}
            .fact(v-if="isManaged(certificate.status)")
              span.label Allowed suffixes
              strong {{ certificate.status.allowedDnsSuffixes.length ? certificate.status.allowedDnsSuffixes.join(', ') : 'None' }}
            .fact(v-if="certificate.status")
              span.label Expires
              strong {{ expiryLabel(certificate.status) }}
            .fact(v-if="certificate.status")
              span.label Active content
              code {{ activeContentRevision(certificate.status) }}
            .fact(v-if="archiveRevision(certificate.status) !== null")
              span.label Active archive
              strong {{ archiveRevision(certificate.status) }}
            .fact(v-if="isManaged(certificate.status)")
              span.label Next action
              strong {{ unixSecondsLabel(certificate.status.nextActionUnixSeconds) }}
            .fact(v-if="certificate.status?.lastErrorCode")
              span.label Last error code
              strong.warning-copy {{ certificate.status.lastErrorCode }}
          .action-row
            button.secondary-button(
              v-if="certificate.source === 'certbot' || certificate.source === 'acme_managed'"
              type="button"
              :disabled="!canMutate || mutating !== null"
              @click="reconcile(certificate.name)"
            ) Reconcile status
            button.primary-button(
              v-if="certificate.source === 'acme_managed'"
              type="button"
              :disabled="!canMutate || mutating !== null"
              @click="renew(certificate.name)"
            ) Renew now
            span.read-only-note(v-if="certificate.developmentOnly") Development-only identity

    section.jobs-panel(aria-labelledby="jobs-heading")
      header.panel-heading
        div
          p.eyebrow Bounded event history
          h3#jobs-heading Certificate jobs
        span.panel-note {{ certificateJobs.length }}
      p.empty-list(v-if="certificateJobs.length === 0") No certificate job events are retained in the current event ring.
      .job-list(v-else)
        article.job-row(v-for="job in certificateJobs" :key="job.cursor")
          .job-cursor
            strong {{ job.cursor }}
            span {{ job.timestampUnixMs === null ? 'No timestamp' : formatTime(job.timestampUnixMs) }}
          .job-content
            strong {{ job.certificate }}
            span {{ job.event.replaceAll('_', ' ') }} / {{ job.outcome }}
            code(v-if="job.revision") {{ shortRevision(job.revision) }}
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import {
  ApiError,
  connectEventStream,
  fetchEvents,
  fetchGenerations,
  fetchTlsInventory,
  reconcileTls,
  renewManagedCertificate,
  type EventStreamClient,
  type OperationalEvent,
  type TlsCertificateInventory,
  type TlsInventory,
  type TlsManagedCertificateStatus,
  type TlsMaterialStatus,
} from './api'
import { formatCount } from './formatters'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const inventory = ref<TlsInventory | null>(null)
const generationRevision = ref<string | null>(null)
const jobs = ref<OperationalEvent[]>([])
const loading = ref(false)
const mutating = ref<string | null>(null)
const error = ref<string | null>(null)
const message = ref<string | null>(null)
let controller: AbortController | null = null
let stream: EventStreamClient | null = null

const canMutate = computed(() => Boolean(props.token && generationRevision.value))
const certificateJobs = computed(() => jobs.value
  .filter((event) => event.certificate && ['certificate_renewal', 'certificate_activated'].includes(event.event))
  .sort((left, right) => right.cursor - left.cursor))

async function load(): Promise<void> {
  if (!props.token || loading.value) return
  controller?.abort()
  const nextController = new AbortController()
  controller = nextController
  loading.value = true
  error.value = null
  try {
    const [nextInventory, nextGeneration, eventPage] = await Promise.all([
      fetchTlsInventory(props.token, nextController.signal),
      fetchGenerations(props.token, nextController.signal),
      fetchEvents(0, 100, props.token, nextController.signal),
    ])
    if (nextController.signal.aborted) return
    inventory.value = nextInventory
    generationRevision.value = nextGeneration.generation.activeRevision
    jobs.value = eventPage.events
  } catch (requestError) {
    if (nextController.signal.aborted) return
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    error.value = errorMessage(requestError, 'The certificate API did not respond.')
  } finally {
    if (controller === nextController) controller = null
    loading.value = false
  }
}

async function runMutation(
  label: string,
  action: (revision: string, token: string) => Promise<unknown>,
): Promise<void> {
  const revision = generationRevision.value
  if (!props.token || !revision || !window.confirm(`Confirm ${label}? Active revision ${shortRevision(revision)} will be checked.`)) return
  mutating.value = label
  error.value = null
  message.value = null
  try {
    const result = await action(revision, props.token)
    message.value = `${label} completed.`
    if (result && typeof result === 'object' && 'outcome' in result) {
      message.value = `${label} completed with outcome ${(result as { outcome: string }).outcome}.`
    }
    await load()
  } catch (requestError) {
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    if (requestError instanceof ApiError && requestError.status === 409) await load()
    error.value = errorMessage(requestError, `${label} failed.`)
  } finally {
    mutating.value = null
  }
}

function reconcile(certificate: string): Promise<void> {
  return runMutation(`reconcile ${certificate}`, (revision, token) => reconcileTls(revision, token, certificate))
}

function renew(certificate: string): Promise<void> {
  return runMutation(`renew ${certificate}`, (revision, token) => renewManagedCertificate(revision, token, certificate))
}

function isManaged(status: TlsMaterialStatus | TlsManagedCertificateStatus | null): status is TlsManagedCertificateStatus {
  return status !== null && 'directoryUrl' in status
}

function certificateStatusLabel(certificate: TlsCertificateInventory): string {
  if (!certificate.status) return 'No active material'
  if (certificate.status.lastErrorCode) return 'Error reported'
  return certificate.developmentOnly ? 'Development only' : 'Loaded'
}

function certificateStatusClass(certificate: TlsCertificateInventory): string {
  if (!certificate.status || certificate.status.lastErrorCode) return 'status-alert'
  return certificate.developmentOnly ? 'status-development' : 'status-healthy'
}

function activeContentRevision(status: TlsMaterialStatus | TlsManagedCertificateStatus): string {
  return isManaged(status) ? shortRevision(status.activeRevision) : shortRevision(status.activeContentRevision)
}

function archiveRevision(status: TlsMaterialStatus | TlsManagedCertificateStatus | null): number | null {
  return status !== null && !isManaged(status) ? status.activeArchiveRevision ?? null : null
}

function expiryLabel(status: TlsMaterialStatus | TlsManagedCertificateStatus): string {
  return isManaged(status) ? status.notAfter : status.expiresAt
}

function unixSecondsLabel(value: number | null): string {
  return value === null ? 'Not scheduled' : formatTime(value * 1000)
}

function shortRevision(value: string): string {
  return value.length > 16 ? `${value.slice(0, 12)}...${value.slice(-4)}` : value
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'short',
    timeStyle: 'medium',
  }).format(timestamp)
}

function errorMessage(value: unknown, fallback: string): string {
  return value instanceof Error ? value.message : fallback
}

function startStream(): void {
  if (!props.token || stream) return
  const client = connectEventStream(props.token, {
    onEvent: (event) => {
      if (event.certificate) jobs.value = [...jobs.value.filter(({ cursor }) => cursor !== event.cursor), event]
    },
    onResyncRequired: async () => {
      await load()
    },
    onError: (streamError) => {
      if (streamError instanceof ApiError && streamError.status === 401) emit('unauthorized')
    },
  })
  stream = client
  void client.closed.then(() => {
    if (stream === client) stream = null
  })
}

function closeStream(): void {
  stream?.close()
  stream = null
}

watch(() => props.token, async (token) => {
  closeStream()
  controller?.abort()
  inventory.value = null
  generationRevision.value = null
  jobs.value = []
  error.value = null
  message.value = null
  if (!token) return
  await load()
  startStream()
})

onMounted(async () => {
  if (!props.token) return
  await load()
  startStream()
})

onBeforeUnmount(() => {
  closeStream()
  controller?.abort()
})
</script>

<style scoped>
.certificates-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.section-heading,
.panel-heading,
.certificate-heading,
.action-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.workspace-heading,
.section-heading {
  align-items: flex-end;
}

.workspace-heading {
  margin-bottom: 28px;
}

.eyebrow,
.label {
  margin: 0;
  color: #929a88;
  font: 700 0.66rem/1.2 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
h4,
p {
  margin-top: 0;
}

h2,
h3,
h4 {
  font-family: Georgia, "Times New Roman", serif;
  font-weight: 400;
  letter-spacing: -0.04em;
}

h2 {
  margin: 4px 0 0;
  font-size: clamp(2.2rem, 5vw, 4rem);
}

h3 {
  margin: 4px 0 0;
  font-size: clamp(1.35rem, 3vw, 2.1rem);
}

h4 {
  margin: 7px 0 0;
  font-size: 1.35rem;
}

.workspace-deck {
  max-width: 700px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.watcher-panel,
.inventory-panel,
.jobs-panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 220px;
  padding: clamp(24px, 5vw, 48px);
}

.auth-panel p,
.loading-panel p {
  margin: 7px 0 0;
  color: #8e9686;
}

.capability-index {
  color: #ffbf4b;
  font: clamp(3.5rem, 9vw, 7rem) Georgia, "Times New Roman", serif;
}

.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: certificate-spin 800ms linear infinite;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
}

.error-notice {
  border-color: #ff745c;
}

.success-notice {
  border-color: #b6ff51;
}

.watcher-panel,
.inventory-panel,
.jobs-panel {
  margin-top: 22px;
}

.watcher-panel {
  padding: 22px;
}

.status-chip,
.source-chip {
  display: inline-block;
  padding: 5px 8px;
  border: 1px solid #536544;
  color: #c7ef94;
  font: 700 0.63rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: uppercase;
}

.status-alert,
.watcher-degraded,
.watcher-stopped {
  border-color: #81483f;
  color: #ff9b88;
}

.status-development {
  border-color: #806f47;
  color: #ffcf70;
}

.watcher-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 1px;
  margin-top: 18px;
  background: #34392f;
}

.watcher-metric {
  display: grid;
  gap: 8px;
  padding: 15px;
  background: #0e110d;
}

.watcher-metric strong {
  color: #dce3d4;
}

.panel-heading {
  align-items: flex-end;
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.panel-note,
.empty-list,
.read-only-note {
  color: #8e9686;
  font-size: 0.72rem;
}

.empty-list {
  margin: 0;
  padding: 22px 20px;
}

.certificate-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 360px), 1fr));
  gap: 1px;
  background: #34392f;
}

.certificate-card {
  min-width: 0;
  padding: 20px;
  background: #10130e;
}

.certificate-heading {
  align-items: flex-start;
}

.source-chip {
  border-color: #5b7269;
  color: #a7ded0;
}

.certificate-facts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 1px;
  margin-top: 18px;
  background: #34392f;
}

.fact {
  display: grid;
  gap: 7px;
  min-width: 0;
  padding: 13px;
  background: #0e110d;
}

.fact strong,
.fact code {
  overflow-wrap: anywhere;
  color: #dce3d4;
  font-size: 0.76rem;
  font-weight: 500;
}

.fact code {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.warning-copy {
  color: #ff9b88 !important;
}

.action-row {
  justify-content: flex-start;
  flex-wrap: wrap;
  margin-top: 16px;
}

.primary-button,
.secondary-button {
  min-height: 41px;
  padding: 10px 14px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font-weight: 700;
}

.primary-button {
  border-color: #b6ff51;
  color: #11150c;
  background: #b6ff51;
}

button:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

.job-list {
  display: grid;
}

.job-row {
  display: grid;
  grid-template-columns: 100px minmax(0, 1fr);
  gap: 18px;
  padding: 17px 20px;
  border-bottom: 1px solid #292e26;
}

.job-cursor {
  display: grid;
  gap: 5px;
}

.job-cursor strong {
  color: #b6ff51;
  font: 0.85rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.job-cursor span,
.job-content span,
.job-content code {
  color: #8e9686;
  font-size: 0.72rem;
}

.job-content {
  display: grid;
  gap: 6px;
}

.job-content code {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

button:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

@keyframes certificate-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 760px) {
  .workspace-heading,
  .section-heading,
  .certificate-heading,
  .panel-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .watcher-grid,
  .certificate-facts {
    grid-template-columns: 1fr;
  }

  .auth-panel,
  .loading-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .job-row {
    grid-template-columns: 1fr;
    gap: 9px;
  }

  .action-row {
    align-items: stretch;
    flex-direction: column;
  }

  .action-row button {
    width: 100%;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
