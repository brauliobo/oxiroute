<template lang="pug">
section.config-workspace(ref="workspaceRoot" aria-labelledby="configuration-heading" :aria-busy="unlocking || loading || validating || saving" @keydown.esc="closeReview")
  WorkspaceHeading(:can-check="snapshot !== null && accessToken !== null" :loading="loading" :review-open="reviewOpen" @check="checkDiskRevision")

  UnlockPanel(v-if="!accessToken" :unlocking="unlocking" :error="unlockError" @unlock="unlockConfiguration")

  LoadingPanel(
    v-else-if="loading && !snapshot"
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
      :review-open="reviewOpen"
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
      :review-open="reviewOpen"
      @discard="discardAndReload"
    )

    .config-layout(:inert="reviewOpen || undefined")
      ObjectRail(
        v-model="selectedKey"
        :config="draft"
        :groups="navigationGroups"
        :options="objectOptions"
        :compositional="snapshot.compositional"
        @add="addObject"
      )

      main.editor-surface
        form.editor-form(:inert="snapshot.compositional || undefined" @submit.prevent @input="markDraftChanged" @change="markDraftChanged")
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

        EditorActions(
          :dirty="isDirty"
          :validating="validating"
          :saving="saving"
          :stale="staleRevision !== null"
          :compositional="snapshot.compositional"
          :validation-current="validationCurrent"
          :can-review="canReviewSave"
          @reset="resetDraft"
          @validate="runValidation"
          @review="openReview"
        )

    DiagnosticsSection(
      :diagnostics="visibleDiagnostics"
      :context="diagnosticContext"
      :review-open="reviewOpen"
      @focus-field="focusDiagnostic"
    )

    ValidationOutput(v-if="validationCurrent && validationResult" :result="validationResult" :review-open="reviewOpen")

    SaveReviewDialog(
      v-if="reviewOpen"
      :disk-revision="diskRevision"
      :active-revision="activeRevision"
      :result="validationResult"
      :normalization-changed="normalizationChanged"
      :error="dialogError"
      :saving="saving"
      :can-save="canReviewSave && staleRevision === null"
      @close="closeReview"
      @save="writeCandidate"
    )
</template>

<script setup lang="ts" vapor>
import { nextTick, onActivated, onBeforeUnmount, onDeactivated, onMounted, ref } from 'vue'

import type { CanonicalConfig, ConfigDiagnostic, ConfigSnapshot } from './config'
import CacheStoreEditor from './configuration/CacheStoreEditor.vue'
import CertificateEditor from './configuration/CertificateEditor.vue'
import { clone } from './configuration/clone'
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
import RevisionBanners from './configuration/workspace/RevisionBanners.vue'
import RevisionBoard from './configuration/workspace/RevisionBoard.vue'
import SaveReviewDialog from './configuration/workspace/SaveReviewDialog.vue'
import StatsSection from './configuration/workspace/StatsSection.vue'
import UnlockPanel from './configuration/workspace/UnlockPanel.vue'
import { useDiagnosticFocus } from './configuration/workspace/useDiagnosticFocus'
import { useReviewDialog } from './configuration/workspace/useReviewDialog'
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
const accessToken = ref<string | null>(null)
const unlockError = ref<string | null>(null)
const unlocking = ref(false)
const selectedKey = ref('general')

const { reviewOpen, enterReview, exitReview, releaseBackground } = useReviewDialog(workspaceRoot)

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

async function unlockConfiguration(token: string): Promise<void> {
  accessToken.value = token
  unlockError.value = null
  unlocking.value = true
  try {
    await loadSnapshot(snapshot.value !== null && isDirty.value)
  } finally {
    unlocking.value = false
  }
}

function ensureSelection(): void {
  if (!selectionExists(selectedKey.value)) selectedKey.value = 'general'
}

function checkDiskRevision(): void {
  void loadSnapshot(true)
}

function discardAndReload(): void {
  if (isDirty.value && !window.confirm('Discard this draft and load the configuration from disk?')) return
  void loadSnapshot(false)
}

function markDraftChanged(): void {
  clearMessages()
  tlsAlpnMessage.value = null
}

function resetDraft(): void {
  if (!snapshot.value) return
  if (staleRevision.value) {
    if (!window.confirm('The disk revision changed. Discard this draft and reload the authoritative configuration?')) return
    void loadSnapshot(false)
    return
  }
  if (isDirty.value && !window.confirm('Discard all unsaved configuration changes?')) return
  draft.value = clone(snapshot.value.config)
  resetForSnapshot()
  ensureSelection()
}

function closeReview(): void {
  if (!reviewOpen.value || saving.value) return
  exitReview()
}

function openReview(event: Event): void {
  if (!canReviewSave.value) return
  dialogError.value = null
  enterReview(event)
}

function relockConfiguration(): void {
  stopLoading()
  accessToken.value = null
  unlockError.value = 'Authorization expired or was rejected. Enter a valid bearer token to continue.'
  abortRequests()
  loading.value = false
  if (reviewOpen.value) exitReview('#config-access-token')
  else void nextTick(() => document.querySelector<HTMLInputElement>('#config-access-token')?.focus())
}

function warnBeforeUnload(event: BeforeUnloadEvent): void {
  if (!isDirty.value) return
  event.preventDefault()
  event.returnValue = ''
}

onMounted(() => {
  window.addEventListener('beforeunload', warnBeforeUnload)
  ensureEventStream()
})
onActivated(() => {
  ensureEventStream()
})
onDeactivated(() => {
  stopLoading()
  abortRequests()
})
onBeforeUnmount(() => {
  window.removeEventListener('beforeunload', warnBeforeUnload)
  releaseBackground()
  stopLoading()
  abortRequests()
})
</script>

<style src="./configuration/editor.css"></style>

<style scoped>
.config-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.config-layout {
  display: grid;
  grid-template-columns: 250px minmax(0, 1fr);
  min-height: 680px;
  margin-top: 28px;
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 82%);
}

.editor-surface {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

.editor-form {
  flex: 1;
  padding: clamp(22px, 4vw, 42px);
}

@media (max-width: 900px) {
  .config-layout {
    grid-template-columns: 210px minmax(0, 1fr);
  }
}

@media (max-width: 700px) {
  .config-layout {
    display: block;
    min-height: 0;
  }
}

@media (max-width: 440px) {
  .editor-form {
    padding-inline: 15px;
  }
}
</style>
