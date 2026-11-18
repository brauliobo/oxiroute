<template lang="pug">
section.config-workspace(ref="workspaceRoot" aria-labelledby="configuration-heading" :aria-busy="loading || validating || saving")
  WorkspaceHeading(:can-check="snapshot !== null && accessToken !== null" :loading="loading" @check="checkDiskRevision")

  LoadingPanel(
    v-if="loading && !snapshot"
    title="Reading canonical configuration"
    detail="No editor state is created until the control-plane capability responds."
  )

  FailurePanel(
    v-else-if="capabilityUnavailable"
    role="status"
    code="404"
    title="Configuration capability unavailable"
    :message="capabilityUnavailable"
    note="The runtime observatory remains available. No placeholder configuration has been created."
    retry-label="Retry capability check"
    @retry="loadSnapshot(false)"
  )

  CanonicalUnavailablePanel(v-else-if="canonicalUnavailable && !snapshot" :state="canonicalUnavailable" @retry="loadSnapshot(false)")

  FailurePanel.load-error-panel(
    v-else-if="loadError"
    role="alert"
    code="ERR"
    title="Configuration response could not be loaded"
    :message="loadError"
    note="No editable state was created from an absent or malformed response."
    retry-label="Retry configuration load"
    @retry="loadSnapshot(false)"
  )

  template(v-else-if="snapshot && draft")
    RevisionBoard(
      :disk-revision="diskRevision"
      :active-revision="activeRevision"
      :dirty="isDirty"
      :validation-current="validationCurrent"
    )

    RevisionBanners(
      :snapshot="snapshot"
      :disk-revision="diskRevision"
      :active-revision="activeRevision"
      :stale-revision="staleRevision"
      :refresh-error="refreshError"
      :canonical-unavailable="canonicalUnavailable"
      :save-message="saveMessage"
      :tls-alpn-message="tlsAlpnMessage"
      @discard="discardAndReload"
      @reapply="reloadAndReapply"
    )

    ReapplyReport(v-if="reapplyReport" :report="reapplyReport" @dismiss="reapplyReport = null")

    .config-layout
      ObjectRail(
        v-model="selectedKey"
        :config="draft"
        :groups="navigationGroups"
        :options="objectOptions"
        :compositional="snapshot.compositional"
        @add="addObject"
      )

      .editor-surface
        form.cfg-editor-form(:inert="snapshot.compositional || undefined" @submit.prevent @input="markDraftChanged" @change="markDraftChanged")
          GeneralSection(v-if="selectedKey === 'general'" :config="draft")
          ManagementSection(v-else-if="selectedKey === 'management'" :config="draft")
          StatsSection(v-else-if="selectedKey === 'stats'" :config="draft")
          CertificateEditor(
            v-else-if="selectedCertificate"
            :certificate="selectedCertificate"
            @remove="removeSelected('certificates')"
            @prepare-tls-alpn="prepareTlsAlpnListener"
          )
          TlsProfileEditor(
            v-else-if="selectedTlsProfile"
            :profile="selectedTlsProfile"
            :certificate-names="certificateNames"
            @remove="removeSelected('tls_profiles')"
          )
          ListenerEditor(
            v-else-if="selectedListener"
            :listener="selectedListener"
            :http-service-names="httpServiceNames"
            :rtmp-service-names="rtmpServiceNames"
            :l4-service-names="l4ServiceNames"
            :forward-proxy-services="forwardProxyServices"
            :tls-profiles="draft.tls_profiles"
            @remove="removeSelected('listeners')"
          )
          CacheStoreEditor(
            v-else-if="selectedCacheStore"
            :store="selectedCacheStore"
            @replace="replaceSelectedCacheStore"
            @remove="removeSelected('cache_stores')"
          )
          UpstreamPoolEditor(
            v-else-if="selectedPool"
            :pool="selectedPool"
            :l4-services="draft.l4_services"
            @changed="markDraftChanged"
            @remove="removeSelected('upstream_pools')"
          )
          HttpServiceEditor(
            v-else-if="selectedHttpService"
            :service="selectedHttpService"
            :pool-names="poolNames"
            :cache-store-names="cacheStoreNames"
            @changed="markDraftChanged"
            @remove="removeSelected('http_services')"
          )
          L4ServiceEditor(
            v-else-if="selectedL4Service"
            :service="selectedL4Service"
            :pool-names="l4PoolNames"
            @remove="removeSelected('l4_services')"
          )
          ForwardProxyServiceEditor(
            v-else-if="selectedForwardProxyService"
            :service="selectedForwardProxyService"
            :cache-store-names="cacheStoreNames"
            @remove="removeSelected('forward_proxy_services')"
          )
          RtmpServiceEditor(
            v-else-if="selectedRtmpService"
            :service="selectedRtmpService"
            @changed="markDraftChanged"
            @remove="removeSelected('rtmp_services')"
          )

        DiagnosticsSection(:placed="placedDiagnostics" :context="diagnosticContext" @focus-field="focusDiagnostic")

        EditorActions(
          :dirty="isDirty"
          :validating="validating"
          :saving="saving"
          :stale="staleRevision !== null"
          :compositional="snapshot.compositional"
          :validation-current="validationCurrent"
          :can-review="canReviewSave"
          :summary="actionSummary"
          @reset="resetDraft"
          @validate="validate"
          @review="openReview"
        )

    ValidationOutput(v-if="validationCurrent && validationResult" :result="validationResult")

    SaveReviewDialog(
      :open="reviewOpen"
      :disk-revision="diskRevision"
      :active-revision="activeRevision"
      :result="validationResult"
      :diff="reviewDiff"
      :normalization-changed="normalizationChanged"
      :error="dialogError"
      :saving="saving"
      :can-save="canReviewSave && staleRevision === null"
      @close="closeReview"
      @save="writeCandidate"
    )
</template>

<script setup lang="ts" vapor>
import { computed, nextTick, onActivated, onBeforeUnmount, onDeactivated, onMounted, ref, watch } from 'vue'

import { locateDiagnostic, type CanonicalConfig, type ConfigDiagnostic, type ConfigSnapshot } from './config'
import CacheStoreEditor from './configuration/CacheStoreEditor.vue'
import CertificateEditor from './configuration/CertificateEditor.vue'
import { clone } from './configuration/clone'
import { diffLines } from './configuration/diff/lines'
import { changesBetween } from './configuration/merge/tree'
import { pathSummary } from './configuration/merge/summary'
import { mergeConfig, type MergeResult } from './configuration/merge/threeWay'
import { issueId, provideFieldDiagnostics } from './configuration/fields/context'
import ForwardProxyServiceEditor from './configuration/ForwardProxyServiceEditor.vue'
import HttpServiceEditor from './configuration/HttpServiceEditor.vue'
import L4ServiceEditor from './configuration/L4ServiceEditor.vue'
import ListenerEditor from './configuration/ListenerEditor.vue'
import RtmpServiceEditor from './configuration/RtmpServiceEditor.vue'
import TlsProfileEditor from './configuration/TlsProfileEditor.vue'
import UpstreamPoolEditor from './configuration/UpstreamPoolEditor.vue'
import { useConfigurationLifecycle } from './configuration/useConfigurationLifecycle'
import { useConfigurationNavigation } from './configuration/useConfigurationNavigation'
import CanonicalUnavailablePanel from './configuration/workspace/CanonicalUnavailablePanel.vue'
import DiagnosticsSection from './configuration/workspace/DiagnosticsSection.vue'
import EditorActions from './configuration/workspace/EditorActions.vue'
import FailurePanel from './configuration/workspace/FailurePanel.vue'
import GeneralSection from './configuration/workspace/GeneralSection.vue'
import LoadingPanel from './configuration/workspace/LoadingPanel.vue'
import ManagementSection from './configuration/workspace/ManagementSection.vue'
import ObjectRail from './configuration/workspace/ObjectRail.vue'
import ReapplyReport from './configuration/workspace/ReapplyReport.vue'
import RevisionBanners from './configuration/workspace/RevisionBanners.vue'
import RevisionBoard from './configuration/workspace/RevisionBoard.vue'
import SaveReviewDialog from './configuration/workspace/SaveReviewDialog.vue'
import StatsSection from './configuration/workspace/StatsSection.vue'
import { hasErrors, summarizeDiagnostics } from './configuration/workspace/diagnosticSummary'
import { useDiagnosticFocus } from './configuration/workspace/useDiagnosticFocus'
import { useLiveRegion } from '#design/composables/useLiveRegion'
import './configuration/editor.css'
import './configuration/workspace/chrome.css'
import { confirmAction } from './shell/confirm'
import { useSnapshotLoader } from './configuration/workspace/useSnapshotLoader'
import { useTlsAlpnPreparation } from './configuration/workspace/useTlsAlpnPreparation'
import ValidationOutput from './configuration/workspace/ValidationOutput.vue'
import WorkspaceHeading from './configuration/workspace/WorkspaceHeading.vue'

const snapshot = ref<ConfigSnapshot | null>(null)
const workspaceRoot = ref<HTMLElement | null>(null)
const draft = ref<CanonicalConfig | null>(null)
// Vapor components keep their setup state closed; tests read the draft through the mount helper's `vm`.
defineExpose({ draft })
const diskRevision = ref('')
const activeRevision = ref<string | null>(null)
const diskDiagnostics = ref<ConfigDiagnostic[]>([])
const staleRevision = ref<string | null>(null)
const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()
const accessToken = computed(() => props.token || null)
const selectedKey = ref('general')
const reapplyReport = ref<Pick<MergeResult<unknown>, 'applied' | 'collisions'> | null>(null)

const reviewOpen = ref(false)

const {
  canReviewSave,
  diagnosticContext,
  dialogError,
  isDirty,
  normalizationChanged,
  saveMessage,
  saving,
  validating,
  validationCurrent,
  validationResult,
  visibleDiagnostics,
  abortRequests,
  clearMessages,
  resetForSnapshot,
  syncSnapshot,
  runValidation,
  writeCandidate,
} = useConfigurationLifecycle({
  draft,
  snapshot,
  diskRevision,
  activeRevision,
  diskDiagnostics,
  staleRevision,
  accessToken,
  onUnauthorized: relockConfiguration,
  onExitReview: exitReview,
  onSaved: () => { reapplyReport.value = null },
})

const {
  certificateNames,
  cacheStoreNames,
  forwardProxyServices,
  httpServiceNames,
  l4PoolNames,
  l4ServiceNames,
  navigationGroups,
  objectOptions,
  poolNames,
  rtmpServiceNames,
  selectedCertificate,
  selectedCacheStore,
  selectedForwardProxyService,
  selectedHttpService,
  selectedL4Service,
  selectedListener,
  selectedPool,
  selectedRtmpService,
  selectedTlsProfile,
  selectionExists,
  addObject,
  replaceSelectedCacheStore,
  removeSelected,
} = useConfigurationNavigation(draft, selectedKey, markDraftChanged)

const { tlsAlpnMessage, prepareTlsAlpnListener } = useTlsAlpnPreparation({
  draft,
  selectedCertificate,
  selectedKey,
  markDraftChanged,
})

const {
  loading,
  capabilityUnavailable,
  canonicalUnavailable,
  loadError,
  refreshError,
  loadSnapshot,
  ensureEventStream,
  stop: stopLoading,
} = useSnapshotLoader({
  accessToken,
  snapshot,
  draft,
  diskDiagnostics,
  staleRevision,
  syncSnapshot,
  ensureSelection,
  onUnauthorized: relockConfiguration,
})

const focusDiagnostic = useDiagnosticFocus(workspaceRoot, selectedKey)
const { announce } = useLiveRegion()

const placedDiagnostics = computed(() => visibleDiagnostics.value.map((diagnostic, index) => ({
  id: issueId(index),
  diagnostic,
  location: draft.value && diagnostic.path ? locateDiagnostic(draft.value, diagnostic.path) : null,
})))
const actionSummary = computed(() => visibleDiagnostics.value.length > 0
  ? summarizeDiagnostics(visibleDiagnostics.value)
  : validationCurrent.value ? 'Validated: no diagnostics' : '')
provideFieldDiagnostics({ placed: placedDiagnostics, selectedKey })

async function validate(): Promise<void> {
  await runValidation()
  const diagnostics = visibleDiagnostics.value
  announce(`Validation finished: ${summarizeDiagnostics(diagnostics)}.`, hasErrors(diagnostics) ? 'assertive' : 'polite')
  if (diagnostics.length === 0) return
  await nextTick()
  const heading = workspaceRoot.value?.querySelector<HTMLElement>('#diagnostics-heading')
  heading?.focus()
  heading?.scrollIntoView?.({ block: 'center' })
}

function ensureSelection(): void {
  if (!selectionExists(selectedKey.value)) selectedKey.value = 'general'
}

function checkDiskRevision(): void {
  void loadSnapshot(true)
}

async function confirmDiscard(title: string): Promise<boolean> {
  const changes = snapshot.value && draft.value ? changesBetween(snapshot.value.config, draft.value) : []
  if (changes.length === 0) return true
  return confirmAction({
    title,
    message: `${changes.length} unsaved ${changes.length === 1 ? 'change is' : 'changes are'} thrown away:`,
    details: pathSummary(changes),
    confirmLabel: 'Discard changes',
    danger: true,
  })
}

async function discardAndReload(): Promise<void> {
  if (!await confirmDiscard('Discard your draft and load the disk file?')) return
  reapplyReport.value = null
  await loadSnapshot(false)
}

async function reloadAndReapply(): Promise<void> {
  const previous = snapshot.value
  const mine = draft.value && clone(draft.value)
  if (!previous || !mine) return
  await loadSnapshot(false)
  if (!snapshot.value || snapshot.value === previous) return
  const { merged, applied, collisions } = mergeConfig(previous.config, mine, snapshot.value.config)
  draft.value = merged
  reapplyReport.value = { applied, collisions }
  ensureSelection()
}

function markDraftChanged(): void {
  clearMessages()
  tlsAlpnMessage.value = null
}

async function resetDraft(): Promise<void> {
  if (!snapshot.value) return
  if (staleRevision.value) return discardAndReload()
  if (!await confirmDiscard('Reset the draft to the disk file?')) return
  if (!snapshot.value) return
  reapplyReport.value = null
  draft.value = clone(snapshot.value.config)
  resetForSnapshot()
  ensureSelection()
}

const reviewDiff = computed(() => reviewOpen.value && snapshot.value && validationResult.value
  ? diffLines(snapshot.value.configPreview, validationResult.value.configPreview)
  : [])

function closeReview(): void {
  reviewOpen.value = false
}

async function exitReview(focusSelector?: string): Promise<void> {
  reviewOpen.value = false
  if (!focusSelector) return
  await nextTick()
  workspaceRoot.value?.querySelector<HTMLElement>(focusSelector)?.focus()
}

function openReview(): void {
  if (!canReviewSave.value) return
  dialogError.value = null
  reviewOpen.value = true
}

watch(dialogError, (error) => {
  if (error) reviewOpen.value = true
})

function relockConfiguration(): void {
  stopLoading()
  abortRequests()
  loading.value = false
  exitReview()
  emit('unauthorized')
}

function warnBeforeUnload(event: BeforeUnloadEvent): void {
  if (!isDirty.value) return
  event.preventDefault()
  event.returnValue = ''
}

watch(accessToken, (token) => {
  if (token) void loadSnapshot(snapshot.value !== null && isDirty.value)
}, { immediate: true })

onMounted(() => window.addEventListener('beforeunload', warnBeforeUnload))
onActivated(() => {
  ensureEventStream()
})
onDeactivated(() => {
  stopLoading()
  abortRequests()
})
onBeforeUnmount(() => {
  window.removeEventListener('beforeunload', warnBeforeUnload)
  stopLoading()
  abortRequests()
})
</script>
