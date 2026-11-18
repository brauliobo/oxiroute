<template lang="pug">
WorkspaceSection(
  title="Certificates"
  description="Inventory is redacted. Renewal and reconciliation check the active generation revision and ask for confirmation."
  :busy="loading || mutating"
)
  template(v-slot:actions)
    Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing' : 'Refresh certificates' }}

  Loading(v-if="loading && !inventory" label="Reading the certificate inventory")

  Notice(v-if="error" kind="alert")
    Banner(tone="bad")
      strong Certificate operation unavailable.
      |  {{ error }}

  template(v-if="inventory")
    WatcherPanel(v-if="inventory.watcher" :watcher="inventory.watcher")

    Panel(title="Configured certificates")
      p.ox-panel-note {{ plural(inventory.certificates.length, 'certificate') }}
      EmptyState(v-if="inventory.certificates.length === 0" title="No certificates are configured") Add a certificate to the configuration to see it here.
      .ox-cert-grid(v-else)
        CertificateCard(
          v-for="certificate in inventory.certificates"
          :key="certificate.name"
          :certificate="certificate"
          :now-ms="loadedAtMs"
          :disabled="!generationRevision || mutating"
          @act="(action) => perform(action, certificate.name)"
        )

    Panel(title="Certificate jobs")
      p.ox-panel-note {{ plural(certificateJobs.length, 'job event') }} in the bounded event ring
      EmptyState(v-if="certificateJobs.length === 0" title="No certificate jobs") No certificate job events are retained in the current event ring.
      JobsTable(v-else :jobs="certificateJobs")
</template>

<script setup lang="ts" vapor>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import { pushToast } from '#design/toast/state'

import { ApiError, connectEventStream, fetchEvents, fetchGenerations, fetchTlsInventory } from './api'
import type { EventStreamClient, OperationalEvent, TlsInventory } from './api'
import { CERTIFICATE_ACTIONS } from './certificates/actions'
import type { CertificateAction } from './certificates/actions'
import CertificateCard from './certificates/CertificateCard.vue'
import JobsTable from './certificates/JobsTable.vue'
import WatcherPanel from './certificates/WatcherPanel.vue'
import { sentence } from './events/format'
import { presentApiError, shortRevision } from './formatters'
import { confirmAction } from './shell/confirm'
import { useLatestAbortableTask } from './useLatestAbortableTask'
import { plural } from './workspace/plural'
import WorkspaceSection from './workspace/WorkspaceSection.vue'

const JOB_EVENTS = new Set([
  'certificate_renewal', 'certificate_activated', 'certificate_revocation',
  'certificate_deletion', 'certificate_account_rollover', 'certificate_job_control',
])

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const inventory = ref<TlsInventory | null>(null)
const generationRevision = ref<string | null>(null)
const jobs = ref<OperationalEvent[]>([])
const loadedAtMs = ref(Date.now())
const mutating = ref(false)
const error = ref<string | null>(null)
let stream: EventStreamClient | null = null
const { loading, run: runLoad, cancel: cancelLoad } = useLatestAbortableTask()

const certificateJobs = computed(() => jobs.value
  .filter((event) => event.certificate && JOB_EVENTS.has(event.event))
  .sort((left, right) => right.cursor - left.cursor))

function load(): Promise<boolean> {
  error.value = null
  return runLoad(
    (signal) => Promise.all([
      fetchTlsInventory(props.token, signal),
      fetchGenerations(props.token, signal),
      fetchEvents(0, 100, props.token, signal),
    ]),
    ([nextInventory, nextGeneration, eventPage]) => {
      inventory.value = nextInventory
      generationRevision.value = nextGeneration.generation.activeRevision
      jobs.value = eventPage.events
      loadedAtMs.value = Date.now()
    },
    (requestError) => {
      if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
      error.value = presentApiError(requestError, 'The certificate API did not respond.')
    },
  )
}

function succeeded(phrase: string, result: unknown): void {
  const outcome = result && typeof result === 'object' && 'outcome' in result ? ` with outcome ${String(result.outcome)}` : ''
  pushToast(`${sentence(phrase)} completed${outcome}.`, 'ok', 6000)
}

async function perform(action: CertificateAction, certificate: string): Promise<void> {
  const revision = generationRevision.value
  const spec = CERTIFICATE_ACTIONS[action]
  const phrase = spec.phrase(certificate)
  if (!revision) return
  const confirmed = await confirmAction({
    title: `${sentence(phrase)}?`,
    message: `Active revision ${shortRevision(revision)} will be checked before the change is sent.`,
    confirmLabel: spec.label,
    danger: spec.variant === 'danger',
  })
  if (!confirmed) return
  mutating.value = true
  error.value = null
  try {
    succeeded(phrase, await spec.run(revision, props.token, certificate))
    await load()
  } catch (requestError) {
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    if (requestError instanceof ApiError && requestError.status === 409) await load()
    error.value = presentApiError(requestError, `${sentence(phrase)} failed.`)
  } finally {
    mutating.value = false
  }
}

function startStream(): void {
  if (stream) return
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

async function start(): Promise<void> {
  if (await load()) startStream()
}

watch(() => props.token, () => {
  closeStream()
  cancelLoad()
  inventory.value = null
  generationRevision.value = null
  jobs.value = []
  error.value = null
  void start()
})

onMounted(start)
onBeforeUnmount(closeStream)
</script>
