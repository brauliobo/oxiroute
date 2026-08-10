<template lang="pug">
section.config-workspace(ref="workspaceRoot" aria-labelledby="configuration-heading" :aria-busy="unlocking || loading || validating || saving" @keydown.esc="closeReview")
  header.workspace-heading(:inert="reviewOpen ? '' : null")
    div
      p.eyebrow Canonical control plane
      h2#configuration-heading Configuration workspace
      p.workspace-deck Edit typed objects, validate on the server, then review the source-format preview before writing.
    button.secondary-button(
      v-if="snapshot && accessToken"
      type="button"
      :disabled="loading"
      :title="loading ? 'A configuration read is already in progress.' : undefined"
      @click="checkDiskRevision"
    ) {{ loading ? 'Checking disk revision...' : 'Check disk revision' }}

  form.unlock-panel(
    v-if="!accessToken"
    data-unlock-form
    aria-labelledby="config-unlock-heading"
    @submit.prevent="unlockConfiguration"
  )
    span.capability-index(aria-hidden="true") AUTH
    div
      h3#config-unlock-heading Unlock canonical configuration
      p Enter the management API bearer token. It is held only in memory for this page session.
      label.unlock-label(for="config-access-token") Bearer token
      input#config-access-token(
        v-model="tokenInput"
        type="password"
        autocomplete="off"
        required
        aria-describedby="config-token-note"
      )
      p#config-token-note The token is never placed in browser storage or the URL.
      p.unlock-error(v-if="unlockError" role="alert") {{ unlockError }}
      button.primary-button(type="submit" :disabled="unlocking || tokenInput.length === 0") {{ unlocking ? 'Unlocking...' : 'Unlock configuration' }}

  .loading-panel(v-else-if="loading && !snapshot" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading canonical configuration
      p No editor state is created until the control-plane capability responds.

  .capability-panel(v-else-if="capabilityUnavailable" role="status")
    span.capability-index 404
    div
      h3 Configuration capability unavailable
      p {{ capabilityUnavailable }}
      p The runtime observatory remains available. No placeholder configuration has been created.
      button.secondary-button(type="button" @click="loadSnapshot(false)") Retry capability check

  .capability-panel.canonical-unavailable-panel(v-else-if="canonicalUnavailable && !snapshot" role="alert")
    span.capability-index 503
    div
      h3 Canonical configuration unavailable
      p The route is available, but the persisted canonical file could not be loaded.
      dl.unavailable-revisions
        div
          dt Disk revision
          dd
            code {{ canonicalUnavailable.diskRevision ? shortRevision(canonicalUnavailable.diskRevision) : 'Unknown' }}
        div
          dt Active revision
          dd
            code {{ canonicalUnavailable.activeRevision ? shortRevision(canonicalUnavailable.activeRevision) : 'Unknown' }}
      p(v-if="canonicalUnavailable.diskRevision === null") The disk revision is uncertain; no write can be reviewed until a successful reload.
      ol.diagnostic-list(v-if="canonicalUnavailable.diagnostics.length")
        li.diagnostic(v-for="(diagnostic, index) in canonicalUnavailable.diagnostics" :key="`${diagnostic.code}-${index}`" :class="`severity-${diagnostic.severity}`")
          .diagnostic-code
            span {{ diagnostic.severity }}
            code {{ diagnostic.code }}
          strong {{ diagnostic.message }}
      button.secondary-button(type="button" @click="loadSnapshot(false)") Retry configuration load

  .capability-panel.load-error-panel(v-else-if="loadError" role="alert")
    span.capability-index ERR
    div
      h3 Configuration response could not be loaded
      p {{ loadError }}
      p No editable state was created from an absent or malformed response.
      button.secondary-button(type="button" @click="loadSnapshot(false)") Retry configuration load

  template(v-else-if="snapshot && draft")
    section.revision-board(:inert="reviewOpen ? '' : null" aria-label="Configuration revisions")
      .revision-cell
        span.revision-label Disk revision
        code {{ shortRevision(diskRevision) }}
        span.revision-detail Save precondition
      .revision-cell
        span.revision-label Active revision
        code {{ activeRevision ? shortRevision(activeRevision) : 'None' }}
        span.revision-detail Serving live traffic
      .revision-cell
        span.revision-label Draft state
        strong(:class="{ changed: isDirty }") {{ isDirty ? 'Unsaved changes' : 'In sync with disk' }}
        span.revision-detail {{ validationCurrent ? 'Server validated' : 'Validation required' }}

    .revision-banner.diverged(v-if="diskRevision !== activeRevision" :inert="reviewOpen ? '' : null" role="status" aria-live="polite")
      strong Disk and active revisions differ.
      |  The saved file is not the generation currently serving traffic.

    .revision-banner.compositional(v-if="snapshot.compositional" :inert="reviewOpen ? '' : null" role="status")
      div
        strong Compositional root is read-only.
        p Typed saves are unavailable because this {{ formatLabel(snapshot.configFormat) }} root resolves {{ snapshot.dependencyCount }} {{ snapshot.dependencyCount === 1 ? 'dependency' : 'dependencies' }}. Inspection and server validation remain available without flattening its source files.

    .revision-banner.stale(v-if="staleRevision" :inert="reviewOpen ? '' : null" tabindex="-1" role="alert")
      div
        strong Draft preserved: the disk revision changed.
        p Expected {{ shortRevision(diskRevision) }}, current {{ shortRevision(staleRevision) }}. Reconcile explicitly; this editor will not overwrite the newer file.
      button.secondary-button(type="button" @click="discardAndReload") Discard draft and load disk

    .revision-banner.error(v-if="refreshError" :inert="reviewOpen ? '' : null" role="alert")
      strong Configuration refresh failed.
      |  {{ refreshError }} The current draft and revision state were retained.

    .revision-banner.error(v-if="canonicalUnavailable" :inert="reviewOpen ? '' : null" role="alert")
      strong Persisted canonical configuration unavailable.
      |  Disk revision {{ canonicalUnavailable.diskRevision ? shortRevision(canonicalUnavailable.diskRevision) : 'unknown' }}; active revision {{ canonicalUnavailable.activeRevision ? shortRevision(canonicalUnavailable.activeRevision) : 'unknown' }}.
      ol.diagnostic-list(v-if="canonicalUnavailable.diagnostics.length")
        li.diagnostic(v-for="(diagnostic, index) in canonicalUnavailable.diagnostics" :key="`${diagnostic.code}-${index}`")
          code {{ diagnostic.code }}
          span {{ diagnostic.message }}

    .revision-banner.save-state(v-if="saveMessage" :inert="reviewOpen ? '' : null" :class="saveMessage.kind" tabindex="-1" role="status" aria-live="polite")
      strong {{ saveMessage.title }}
      |  {{ saveMessage.detail }}

    .revision-banner.tls-alpn(v-if="tlsAlpnMessage" :inert="reviewOpen ? '' : null" :class="tlsAlpnMessage.kind" :role="tlsAlpnMessage.kind === 'error' ? 'alert' : 'status'" aria-live="polite")
      strong {{ tlsAlpnMessage.title }}
      |  {{ tlsAlpnMessage.detail }}

    .config-layout(:inert="reviewOpen ? '' : null")
      aside.object-rail
        label.mobile-nav-label(for="mobile-object-navigation") Current object
        select#mobile-object-navigation.mobile-object-nav(v-model="selectedKey" aria-label="Current configuration object")
          option(value="general") General
          option(value="management") Management
          option(value="stats") Statistics
          option(v-for="option in objectOptions" :key="option.key" :value="option.key") {{ option.group }} / {{ option.label }}

        .mobile-add-controls(aria-label="Add configuration object")
          button.mobile-add(
            v-for="group in navigationGroups"
            :key="group.collection"
            type="button"
            :aria-label="`Add ${group.singular}`"
            :disabled="snapshot.compositional"
            :title="snapshot.compositional ? 'Compositional roots are read-only.' : undefined"
            @click="addObject(group.collection)"
          ) + {{ group.singular }}

        nav.object-navigation(aria-label="Configuration objects" @keydown="moveNavigationFocus")
          p.nav-section-label Core
          button.object-link(type="button" :aria-current="selectedKey === 'general' ? 'page' : undefined" @click="selectedKey = 'general'")
            span General
            small Schema 1
          button.object-link(type="button" :aria-current="selectedKey === 'management' ? 'page' : undefined" @click="selectedKey = 'management'")
            span Management
            small {{ draft.management ? 'Configured' : 'Disabled' }}
          button.object-link(type="button" :aria-current="selectedKey === 'stats' ? 'page' : undefined" @click="selectedKey = 'stats'")
            span Statistics
            small {{ draft.stats ? `${draft.stats.binds.length} observability binds / ${draft.stats.pages.length} pages` : 'Disabled' }}

          template(v-for="group in navigationGroups" :key="group.collection")
            .nav-group-heading(:data-field="group.collection")
              p.nav-section-label {{ group.label }}
              button.nav-add(type="button" :aria-label="`Add ${group.singular}`" :disabled="snapshot.compositional" :title="snapshot.compositional ? 'Compositional roots are read-only.' : undefined" @click="addObject(group.collection)") +
            p.nav-empty(v-if="group.items.length === 0") None
            button.object-link(
              v-for="item in group.items"
              :key="item.key"
              type="button"
              :aria-current="selectedKey === item.key ? 'page' : undefined"
              @click="selectedKey = item.key"
            )
              span {{ item.label }}
              small {{ item.detail }}

      main.editor-surface
        form.editor-form(:inert="snapshot.compositional ? '' : null" :aria-readonly="snapshot.compositional" @submit.prevent @input="markDraftChanged" @change="markDraftChanged")
          template(v-if="selectedKey === 'general'")
            header.form-heading
              div
                p.eyebrow Root object
                h3 General
              span.object-path Config
            .field-grid
              label.field(data-field="version")
                span Version
                input(type="number" min="1" step="1" v-model.number="draft.version")
                small Current canonical schema version.

          template(v-else-if="selectedKey === 'management'")
            header.form-heading
              div
                p.eyebrow Optional object
                h3 Management
              span.object-path Config.management
            label.enable-row(data-field="management")
              input(type="checkbox" :checked="draft.management !== null" @change="toggleManagement")
              span Enable the loopback management listener
            .field-grid(v-if="draft.management")
              label.field(data-field="management.bind")
                span Bind address
                input(type="text" v-model="draft.management.bind" placeholder="127.0.0.1:9080")
              label.field(data-field="management.ui_dir")
                span Prebuilt UI directory
                input(type="text" :value="draft.management.ui_dir ?? ''" placeholder="/opt/oxiroute/ui/dist" @input="setManagementUiDir")
                small Optional; empty writes null.

          template(v-else-if="selectedKey === 'stats'")
            header.form-heading
              div
                p.eyebrow Optional object
                h3 Statistics
              span.object-path Config.stats
            label.enable-row(data-field="stats")
              input(type="checkbox" :checked="draft.stats != null" @change="toggleStats")
              span Enable observability binds and/or standalone statistics pages
            .field-grid(v-if="draft.stats")
              label.field(data-field="stats.binds")
                span Bind addresses
                input(type="text" :value="draft.stats.binds.join(', ')" placeholder="127.0.0.1:8404, [::1]:8404" @input="setStatsBinds")
                small Comma-separated IPv4/IPv6 sockets.
              label.field(data-field="stats.admin_token_file")
                span Admin token file
                input(type="text" :value="draft.stats.admin_token_file ?? ''" placeholder="/etc/oxiroute/stats-admin.token" @input="setStatsAdminTokenFile")
                small Optional; without it, administration remains disabled.
            fieldset.route-list(v-if="draft.stats" data-field="stats.pages")
              .route-heading
                legend HAProxy-compatible pages
                button.add-row(
                  type="button"
                  :disabled="draft.stats.binds.length + draft.stats.pages.length >= 8"
                  title="The server allows at most eight statistics binds and pages in total."
                  @click="addStatsPage"
                ) + Add statistics page
              p.empty-list(v-if="draft.stats.pages.length === 0") No standalone statistics pages configured.
              article.route-card(v-for="(page, pageIndex) in draft.stats.pages" :key="pageIndex")
                header.route-card-heading
                  strong Statistics page {{ pageIndex + 1 }}
                  button.danger-link(type="button" :aria-label="`Remove statistics page ${pageIndex + 1}`" @click="removeStatsPage(pageIndex)") Remove
                .field-grid
                  label.field(data-field="stats.pages[].bind")
                    span Bind address
                    input(type="text" v-model="page.bind" placeholder="127.0.0.1:8404")
                  label.field(data-field="stats.pages[].uri_prefix")
                    span URI prefix
                    input(type="text" v-model="page.uri_prefix" placeholder="/stats")
                  label.field(data-field="stats.pages[].refresh_ms")
                    span Refresh (milliseconds)
                    input(type="number" min="1" max="86400000" step="1" v-model.number="page.refresh_ms")
                  label.field(data-field="stats.pages[].admin")
                    span Administration
                    select(v-model="page.admin")
                      option(value="disabled") Disabled
                      option(value="localhost") Localhost only
                  label.field(data-field="stats.pages[].max_connections")
                    span Maximum connections
                    input(type="number" min="1" step="1" :value="page.max_connections ?? ''" placeholder="Unbounded" @input="setStatsPageOptionalInteger(pageIndex, 'max_connections', $event)")
                  label.field(data-field="stats.pages[].downstream_timeouts.client_timeout_ms")
                    span Client timeout (milliseconds)
                    input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.client_timeout_ms ?? ''" placeholder="No timeout" @input="setStatsPageTimeout(pageIndex, 'client_timeout_ms', $event)")
                  label.field(data-field="stats.pages[].downstream_timeouts.request_timeout_ms")
                    span Request-header timeout (milliseconds)
                    input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.request_timeout_ms ?? ''" placeholder="Inherit client timeout" @input="setStatsPageTimeout(pageIndex, 'request_timeout_ms', $event)")
                  label.field(data-field="stats.pages[].downstream_timeouts.keepalive_timeout_ms")
                    span Keep-alive timeout (milliseconds)
                    input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.keepalive_timeout_ms ?? ''" placeholder="Runtime default" @input="setStatsPageTimeout(pageIndex, 'keepalive_timeout_ms', $event)")
                small The page is public and adds no routes beyond its URI prefix. Localhost administration accepts only same-origin loopback requests.

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

        footer.editor-actions
          button.secondary-button(type="button" :disabled="!isDirty || validating || saving" :title="resetDisabledReason" @click="resetDraft") Reset draft
          button.primary-button(type="button" :disabled="validating || saving || staleRevision !== null" :title="validationDisabledReason" @click="runValidation")
            | {{ validating ? 'Validating...' : 'Validate candidate' }}
          button.review-button(type="button" :disabled="!canReviewSave || saving" :title="reviewDisabledReason" @click="openReview") Review save

    section.diagnostics-section(:inert="reviewOpen ? '' : null" aria-labelledby="diagnostics-heading")
      header.output-heading
        div
          p.eyebrow Server analysis
          h3#diagnostics-heading(tabindex="-1") Validation diagnostics
        span.output-count {{ visibleDiagnostics.length }} {{ visibleDiagnostics.length === 1 ? 'item' : 'items' }}
      p.output-empty(v-if="visibleDiagnostics.length === 0") No diagnostics reported for the current {{ diagnosticContext }}.
      ol.diagnostic-list(v-else aria-live="polite")
        li.diagnostic(v-for="(diagnostic, index) in visibleDiagnostics" :key="`${diagnostic.code}-${index}`" :class="`severity-${diagnostic.severity}`")
          .diagnostic-code
            span {{ diagnostic.severity }}
            code {{ diagnostic.code }}
          button.diagnostic-target(v-if="diagnostic.path" type="button" @click="focusDiagnostic(diagnostic.path)")
            strong {{ diagnostic.message }}
            p.diagnostic-meta {{ diagnostic.stage }}{{ diagnostic.path ? ` / ${diagnostic.path}` : '' }}
          div(v-else)
            strong {{ diagnostic.message }}
            p.diagnostic-meta {{ diagnostic.stage }}

    section.validation-output(v-if="validationCurrent && validationResult" :inert="reviewOpen ? '' : null" aria-label="Validated candidate output")
      article.preview-panel
        header.output-heading
          div
            p.eyebrow Backend rendered
            h3 {{ formatLabel(validationResult.configFormat) }} configuration preview
          code {{ shortRevision(validationResult.candidateRevision) }}
        pre(tabindex="0") {{ validationResult.configPreview }}
      article.candidate-topology
        header.output-heading
          div
            p.eyebrow Candidate graph
            h3 Validation topology
          span.output-count {{ validationResult.topology.nodes.length }} nodes / {{ validationResult.topology.edges.length }} edges
        p.candidate-state Candidate only / not active
        ul.candidate-node-list
          li(v-for="node in validationResult.topology.nodes" :key="node.id")
            span {{ node.kind.replaceAll('_', ' ') }}
            strong {{ node.name }}
            code {{ node.configPath }}

    .review-scrim(v-if="reviewOpen" role="presentation" @click.self="closeReview")
      section.save-review(role="dialog" aria-modal="true" aria-labelledby="save-review-heading" @keydown.tab="trapReviewFocus")
        header.review-heading
          div
            p.eyebrow Revision-checked write
            h3#save-review-heading Save review
          button.close-button(type="button" aria-label="Close save review" @click="closeReview") Close
        dl.review-facts
          div
            dt Expected disk revision
            dd
              code {{ shortRevision(diskRevision) }}
          div
            dt Candidate revision
            dd
              code {{ validationResult ? shortRevision(validationResult.candidateRevision) : '--' }}
          div
            dt Active now
            dd
              code {{ activeRevision ? shortRevision(activeRevision) : 'None' }}
          div
            dt Normalization
            dd {{ normalizationChanged ? 'Server normalized the submitted model' : 'No model changes' }}
        p.dialog-error(v-if="dialogError" role="alert") {{ dialogError }}
        p.review-warning(v-if="validationResult?.restartRequired") This active Unix listener mode change is saved for the next process restart.
        p.review-warning(v-else) A changed canonical file is queued for in-process activation; no process restart is required.
        .review-actions
          button.secondary-button(type="button" @click="closeReview") Continue editing
          button.primary-button(type="button" :disabled="saving || !canReviewSave || staleRevision !== null" @click="writeCandidate") {{ saving ? 'Saving...' : 'Save canonical configuration' }}
</template>

<script setup lang="ts">
import { computed, nextTick, onActivated, onBeforeUnmount, onDeactivated, onMounted, ref } from 'vue'

import { ApiError, connectEventStream, fetchConfig, type EventStreamClient } from './api'
import CacheStoreEditor from './configuration/CacheStoreEditor.vue'
import CertificateEditor from './configuration/CertificateEditor.vue'
import ForwardProxyServiceEditor from './configuration/ForwardProxyServiceEditor.vue'
import HttpServiceEditor from './configuration/HttpServiceEditor.vue'
import L4ServiceEditor from './configuration/L4ServiceEditor.vue'
import ListenerEditor from './configuration/ListenerEditor.vue'
import RtmpServiceEditor from './configuration/RtmpServiceEditor.vue'
import TlsProfileEditor from './configuration/TlsProfileEditor.vue'
import UpstreamPoolEditor from './configuration/UpstreamPoolEditor.vue'
import { useConfigurationLifecycle } from './configuration/useConfigurationLifecycle'
import {
  moveConfigurationNavigationFocus,
  useConfigurationNavigation,
} from './configuration/useConfigurationNavigation'
import { errorDiagnosticsFrom } from './config'
import type { CanonicalConfig, ConfigDiagnostic, ConfigSnapshot } from './config'
import { prepareTlsAlpnDeployment } from './configuration/tlsAlpnDeployment'
import { isRecord } from './valueGuards'

interface CanonicalUnavailableState {
  diskRevision: string | null
  activeRevision: string | null
  diagnostics: ConfigDiagnostic[]
}

const snapshot = ref<ConfigSnapshot | null>(null)
const workspaceRoot = ref<HTMLElement | null>(null)
const draft = ref<CanonicalConfig | null>(null)
const diskRevision = ref('')
const activeRevision = ref<string | null>(null)
const diskDiagnostics = ref<ConfigDiagnostic[]>([])
const staleRevision = ref<string | null>(null)
const accessToken = ref<string | null>(null)
const tokenInput = ref('')
const unlockError = ref<string | null>(null)
const unlocking = ref(false)
const loading = ref(false)
const capabilityUnavailable = ref<string | null>(null)
const canonicalUnavailable = ref<CanonicalUnavailableState | null>(null)
const loadError = ref<string | null>(null)
const refreshError = ref<string | null>(null)
const tlsAlpnMessage = ref<{ kind: 'success' | 'error'; title: string; detail: string } | null>(null)
const selectedKey = ref('general')
const reviewOpen = ref(false)
let loadController: AbortController | null = null
let eventStream: EventStreamClient | null = null
let reviewReturnFocus: HTMLElement | null = null

const {
  attemptDiagnostics,
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
  tlsProfileNames,
  addObject,
  replaceSelectedCacheStore,
  removeSelected,
} = useConfigurationNavigation(draft, selectedKey, markDraftChanged)

const moveNavigationFocus = moveConfigurationNavigationFocus
const resetDisabledReason = computed(() => {
  if (validating.value || saving.value) return 'Wait for the current configuration request to finish.'
  if (!isDirty.value) return 'The draft already matches the disk snapshot.'
  return undefined
})
const validationDisabledReason = computed(() => {
  if (staleRevision.value !== null) return 'Reload the changed disk revision before validating this draft.'
  if (validating.value || saving.value) return 'Wait for the current configuration request to finish.'
  return undefined
})
const reviewDisabledReason = computed(() => {
  if (saving.value) return 'A configuration save is in progress.'
  if (snapshot.value?.compositional) return 'Typed saves cannot replace a compositional configuration root.'
  if (staleRevision.value !== null) return 'Reload the changed disk revision before reviewing a save.'
  if (!validationCurrent.value) return 'Validate the current draft on the server before reviewing a save.'
  return undefined
})

async function unlockConfiguration(): Promise<void> {
  const token = tokenInput.value
  if (!token) return
  accessToken.value = token
  tokenInput.value = ''
  unlockError.value = null
  unlocking.value = true
  try {
    await loadSnapshot(snapshot.value !== null && isDirty.value)
  } finally {
    unlocking.value = false
  }
}

async function loadSnapshot(preserveDirty: boolean): Promise<void> {
  const token = accessToken.value
  if (!token) return
  loadController?.abort()
  const controller = new AbortController()
  loadController = controller
  loading.value = true
  capabilityUnavailable.value = null
  loadError.value = null
  try {
    const next = await fetchConfig(token, controller.signal)
    if (controller.signal.aborted) return
    canonicalUnavailable.value = null
    refreshError.value = null
    applySnapshot(next, !preserveDirty)
    ensureEventStream()
  } catch (error) {
    if (controller.signal.aborted) return
    if (error instanceof ApiError && error.status === 401) {
      relockConfiguration()
      return
    }
    const unavailable = canonicalUnavailablePayload(error)
    if (unavailable) {
      canonicalUnavailable.value = unavailable
      diskDiagnostics.value = unavailable.diagnostics
      if (snapshot.value) staleRevision.value = unavailable.diskRevision ?? 'unknown'
      return
    }
    const message = error instanceof Error ? error.message : 'The configuration endpoint did not respond.'
    if (snapshot.value) {
      refreshError.value = message
    } else {
      if (configRouteUnavailable(error)) capabilityUnavailable.value = message
      else loadError.value = message
      draft.value = null
    }
  } finally {
    if (loadController === controller) {
      loadController = null
      loading.value = false
    }
  }
}

function applySnapshot(next: ConfigSnapshot, force = false): void {
  if (!syncSnapshot(next, force)) return
  canonicalUnavailable.value = null
  refreshError.value = null
  if (!selectionExists(selectedKey.value)) selectedKey.value = 'general'
}

function ensureEventStream(): void {
  if (eventStream || !accessToken.value || !snapshot.value) return
  const client = connectEventStream(accessToken.value, {
    onEvent: (event) => {
      if (event.revision !== null) void loadSnapshot(true)
    },
    onResyncRequired: async () => {
      await loadSnapshot(true)
    },
    onError: (error) => {
      if (error instanceof ApiError && error.status === 401) relockConfiguration()
    },
  })
  eventStream = client
  void client.closed.then(() => {
    if (eventStream === client) eventStream = null
  })
}

function stopEventStream(): void {
  eventStream?.close()
  eventStream = null
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

function prepareTlsAlpnListener(): void {
  const config = draft.value
  const certificate = selectedCertificate.value
  if (!config || !certificate) return

  const result = prepareTlsAlpnDeployment(config, certificate.name)
  if (result.outcome === 'blocked') {
    tlsAlpnMessage.value = {
      kind: 'error',
      title: 'TLS-ALPN listener not prepared.',
      detail: result.message,
    }
    return
  }
  if (result.outcome === 'ready') {
    tlsAlpnMessage.value = {
      kind: 'success',
      title: 'TLS-ALPN listener is ready in the draft.',
      detail: `${result.listenerName} uses ${result.profileName} on ${result.bindAddress}. Validate and save the certificate challenge selection; public DNS, firewall, and external reachability remain deployment gates.`,
    }
    return
  }

  markDraftChanged()
  selectedKey.value = `listeners:${result.listenerIndex}`
  tlsAlpnMessage.value = {
    kind: 'success',
    title: 'TLS-ALPN listener draft prepared.',
    detail: `${result.listenerName} will bind ${result.bindAddress} with ${result.profileName} and return 404 for non-challenge HTTP traffic. Nothing is deployed until validation, review, and the revision-checked save.`,
  }
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
  if (!selectionExists(selectedKey.value)) selectedKey.value = 'general'
}

function toggleManagement(event: Event): void {
  if (!draft.value) return
  draft.value.management = (event.target as HTMLInputElement).checked
    ? { bind: '127.0.0.1:9080', ui_dir: null }
    : null
}

function setManagementUiDir(event: Event): void {
  if (draft.value?.management) draft.value.management.ui_dir = nullableInput(event)
}

function toggleStats(event: Event): void {
  if (!draft.value) return
  draft.value.stats = (event.target as HTMLInputElement).checked
    ? { binds: ['127.0.0.1:8404'], admin_token_file: null, pages: [] }
    : null
}

function setStatsBinds(event: Event): void {
  if (!draft.value?.stats) return
  draft.value.stats.binds = (event.target as HTMLInputElement).value
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)
}

function setStatsAdminTokenFile(event: Event): void {
  if (draft.value?.stats) draft.value.stats.admin_token_file = nullableInput(event)
}

function addStatsPage(): void {
  const stats = draft.value?.stats
  if (!stats || stats.binds.length + stats.pages.length >= 8) return
  stats.pages.push({
    bind: '127.0.0.1:8405',
    uri_prefix: '/stats',
    refresh_ms: 10_000,
    admin: 'disabled',
    max_connections: null,
    downstream_timeouts: {
      client_timeout_ms: null,
      request_timeout_ms: null,
      keepalive_timeout_ms: null,
    },
  })
}

function setStatsPageOptionalInteger(
  index: number,
  field: 'max_connections',
  event: Event,
): void {
  const page = draft.value?.stats?.pages[index]
  if (!page) return
  page[field] = nullableIntegerInput(event)
}

function setStatsPageTimeout(
  index: number,
  field: keyof NonNullable<CanonicalConfig['stats']>['pages'][number]['downstream_timeouts'],
  event: Event,
): void {
  const page = draft.value?.stats?.pages[index]
  if (!page) return
  page.downstream_timeouts[field] = nullableIntegerInput(event)
}

function removeStatsPage(index: number): void {
  draft.value?.stats?.pages.splice(index, 1)
}

function closeReview(): void {
  if (!reviewOpen.value || saving.value) return
  exitReview()
}

function openReview(event?: Event): void {
  if (!canReviewSave.value) return
  reviewReturnFocus = event?.currentTarget instanceof HTMLElement
    ? event.currentTarget
    : document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null
  dialogError.value = null
  reviewOpen.value = true
  setSurroundingBackgroundInert(true)
  void nextTick(() =>
    document.querySelector<HTMLButtonElement>('.save-review .close-button')?.focus(),
  )
}

function exitReview(fallbackSelector?: string): void {
  if (!reviewOpen.value) return
  reviewOpen.value = false
  setSurroundingBackgroundInert(false)
  void nextTick(() => {
    const returnTarget = reviewReturnFocus
    reviewReturnFocus = null
    if (
      returnTarget?.isConnected &&
      !(returnTarget instanceof HTMLButtonElement && returnTarget.disabled)
    ) {
      returnTarget.focus()
      return
    }
    document.querySelector<HTMLElement>(fallbackSelector ?? '.review-button')?.focus()
  })
}

function setSurroundingBackgroundInert(inert: boolean): void {
  const workspace = workspaceRoot.value
  const parent = workspace?.parentElement
  if (!workspace || !parent) return
  for (const sibling of parent.children) {
    if (sibling !== workspace) sibling.toggleAttribute('inert', inert)
  }
}

function trapReviewFocus(event: KeyboardEvent): void {
  const dialog = event.currentTarget as HTMLElement
  const controls = Array.from(
    dialog.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'),
  )
  const first = controls[0]
  const last = controls.at(-1)
  if (!first || !last) return
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault()
    first.focus()
  }
}

async function focusDiagnostic(path: string): Promise<void> {
  const indexedObject = path.match(/^([a-z_]+)\[(\d+)\]/) ?? path.match(/^\/([a-z_]+)\/(\d+)/)
  if (indexedObject) selectedKey.value = `${indexedObject[1]}:${indexedObject[2]}`
  else if (path.startsWith('management') || path.startsWith('/management')) selectedKey.value = 'management'
  else if (path.startsWith('stats') || path.startsWith('/stats')) selectedKey.value = 'stats'
  else selectedKey.value = 'general'

  const fieldPath = path.startsWith('/')
    ? path.slice(1).split('/').reduce((result, segment) =>
        /^\d+$/.test(segment) ? `${result}[]` : `${result}${result ? '.' : ''}${segment}`,
      '')
    : path.replace(/\[\d+\]/g, '[]')
  await nextTick()
  const field = Array.from(workspaceRoot.value?.querySelectorAll<HTMLElement>('[data-field]') ?? [])
    .find((candidate) => candidate.dataset.field === fieldPath)
  const control = field?.matches('input, select, textarea, button')
    ? field
    : field?.querySelector<HTMLElement>('input, select, textarea, button')
  control?.focus()
  field?.scrollIntoView?.({ block: 'center' })
}

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement | HTMLSelectElement).value
}

function nullableInput(event: Event): string | null {
  return inputValue(event) || null
}

function nullableIntegerInput(event: Event): number | null {
  const value = inputValue(event)
  return value === '' ? null : Number(value)
}

function shortRevision(revision: string): string {
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

function formatLabel(format: ConfigSnapshot['configFormat']): string {
  return {
    kdl: 'KDL',
    lua: 'Lua',
    uci: 'UCI',
    hocon: 'HOCON',
  }[format]
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function relockConfiguration(): void {
  stopEventStream()
  accessToken.value = null
  tokenInput.value = ''
  unlockError.value = 'Authorization expired or was rejected. Enter a valid bearer token to continue.'
  loadController?.abort()
  abortRequests()
  loading.value = false
  if (reviewOpen.value) exitReview('#config-access-token')
  else void nextTick(() => document.querySelector<HTMLInputElement>('#config-access-token')?.focus())
}

function configRouteUnavailable(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 404) return false
  return error.code === null || error.code === 'route_not_found'
}

function canonicalUnavailablePayload(error: unknown): CanonicalUnavailableState | null {
  if (!(error instanceof ApiError) || error.status !== 503 ||
    error.code !== 'canonical_config_unavailable' || !isRecord(error.payload)
  ) return null
  const payload = error.payload
  return {
    diskRevision: typeof payload.diskRevision === 'string' ? payload.diskRevision : null,
    activeRevision: typeof payload.activeRevision === 'string' ? payload.activeRevision : null,
    diagnostics: errorDiagnosticsFrom(payload),
  }
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
  stopEventStream()
  loadController?.abort()
  abortRequests()
})
onBeforeUnmount(() => {
  window.removeEventListener('beforeunload', warnBeforeUnload)
  setSurroundingBackgroundInert(false)
  stopEventStream()
  loadController?.abort()
  abortRequests()
})
</script>

<style src="./configuration/editor.css"></style>

<style scoped>
.config-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.form-heading,
.output-heading,
.review-heading,
.editor-actions,
.revision-banner,
.route-heading,
.route-card-heading,
.review-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.eyebrow,
.revision-label,
.nav-section-label,
.object-path,
dt {
  margin: 0;
  color: #929a88;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.66rem;
  font-weight: 700;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
p {
  margin-top: 0;
}

h2 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(2.2rem, 5vw, 4.1rem);
  font-weight: 400;
  letter-spacing: -0.05em;
}

h3 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(1.45rem, 3vw, 2.2rem);
  font-weight: 400;
  letter-spacing: -0.035em;
}

.workspace-deck {
  max-width: 680px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.unlock-panel,
.loading-panel,
.capability-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 230px;
  margin-top: 28px;
  padding: clamp(24px, 5vw, 48px);
  border: 1px solid #3a4034;
  background: rgb(20 23 18 / 86%);
}

.unlock-panel {
  border-color: #596b4a;
}

.unlock-panel > div {
  width: min(520px, 100%);
}

.unlock-label {
  display: block;
  margin: 20px 0 7px;
  color: #cbd2c4;
  font-size: 0.78rem;
  font-weight: 700;
}

.unlock-panel input {
  width: 100%;
  min-height: 44px;
  padding: 9px 11px;
  border: 1px solid #59634f;
  color: #eef2e7;
  background: #0d100c;
  font: 0.82rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.unlock-panel input:focus-visible {
  outline: 2px solid #ffffff;
  outline-offset: 2px;
}

.unlock-error,
.dialog-error {
  color: #ff9b88 !important;
}

.loading-panel p,
.unlock-panel p,
.capability-panel p {
  margin: 7px 0 0;
  color: #8e9686;
}

.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: config-spin 800ms linear infinite;
}

.capability-panel {
  border-color: #665941;
}

.capability-index {
  color: #ffbf4b;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(3.5rem, 9vw, 7rem);
}

.revision-board {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  margin-top: 30px;
  border: 1px solid #3a4034;
  background: #121510;
}

.revision-cell {
  display: grid;
  min-width: 0;
  gap: 7px;
  padding: 17px 19px;
}

.revision-cell + .revision-cell {
  border-left: 1px solid #34392f;
}

.revision-cell code,
.revision-cell strong {
  overflow: hidden;
  color: #dce3d4;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.85rem;
  text-overflow: ellipsis;
}

.revision-cell strong.changed {
  color: #ffcf70;
}

.revision-detail {
  color: #70786a;
  font-size: 0.7rem;
}

.revision-banner {
  align-items: flex-start;
  margin-top: 10px;
  padding: 13px 16px;
  border-left: 3px solid #ffbf4b;
  color: #cdd2c5;
  background: #191a14;
}

.revision-banner p {
  margin: 5px 0 0;
}

.revision-banner.error,
.revision-banner.stale {
  border-color: #ff745c;
}

.revision-banner.success {
  border-color: #b6ff51;
}

.config-layout {
  display: grid;
  grid-template-columns: 250px minmax(0, 1fr);
  min-height: 680px;
  margin-top: 28px;
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 82%);
}

.object-rail {
  min-width: 0;
  padding: 18px 0;
  border-right: 1px solid #34392f;
  background: #10130e;
}

.object-navigation {
  display: grid;
}

.nav-section-label {
  padding: 13px 18px 7px;
}

.nav-group-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 9px;
  border-top: 1px solid #292e26;
}

.nav-add {
  width: 32px;
  height: 32px;
  margin: 8px 11px 0 0;
  border: 1px solid #536345;
  color: #b6ff51;
  background: transparent;
  cursor: pointer;
}

.nav-empty {
  margin: 2px 18px 5px;
  color: #60675b;
  font-size: 0.7rem;
}

.object-link {
  position: relative;
  display: grid;
  width: 100%;
  gap: 3px;
  padding: 10px 18px;
  border: 0;
  color: #bec5b6;
  background: transparent;
  cursor: pointer;
  text-align: left;
}

.object-link::before {
  position: absolute;
  inset: 0 auto 0 0;
  width: 2px;
  background: transparent;
  content: "";
}

.object-link:hover,
.object-link[aria-current="page"] {
  color: #fff;
  background: #1b2017;
}

.object-link[aria-current="page"]::before {
  background: #b6ff51;
}

.object-link small {
  color: #70786a;
  font-size: 0.65rem;
}

.mobile-nav-label,
.mobile-object-nav,
.mobile-add-controls {
  display: none;
}

.mobile-add {
  min-height: 44px;
  padding: 8px 10px;
  border: 1px solid #536345;
  color: #c7ef94;
  background: transparent;
  cursor: pointer;
  text-align: left;
}

.mobile-add:focus-visible {
  outline: 2px solid #ffffff;
  outline-offset: 2px;
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

.form-heading {
  align-items: flex-start;
  margin-bottom: 28px;
  padding-bottom: 18px;
  border-bottom: 1px solid #34392f;
}

.object-path {
  text-transform: none;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 18px;
  margin-bottom: 22px;
}

.field {
  display: grid;
  min-width: 0;
  align-content: start;
  gap: 8px;
  color: #b8bfb0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.field small {
  color: #747c6e;
  font-size: 0.68rem;
  font-weight: 400;
  letter-spacing: 0;
  line-height: 1.45;
  text-transform: none;
}

.field input,
.field select,
.mobile-object-nav {
  width: 100%;
  min-width: 0;
  min-height: 41px;
  padding: 9px 11px;
  border: 1px solid #42493c;
  border-radius: 0;
  color: #eef2e7;
  background: #0d100c;
  font: 0.78rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: none;
}

.field input:focus-visible,
.field select:focus-visible,
button:focus-visible,
.mobile-object-nav:focus-visible,
pre:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

.object-block,
.route-list {
  min-width: 0;
  margin: 28px 0 0;
  padding: 20px;
  border: 1px solid #34392f;
  background: rgb(12 15 11 / 54%);
}

.object-block > legend,
.route-list legend {
  padding: 0 8px;
  color: #b6ff51;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.72rem;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.enable-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 20px;
  color: #cbd2c4;
  cursor: pointer;
  font-size: 0.82rem;
}

.enable-row input {
  width: 18px;
  height: 18px;
  accent-color: #b6ff51;
}

.compact-enable {
  min-height: 41px;
  margin: 0;
  padding: 9px 0;
}

.route-list {
  padding-top: 8px;
}

.route-heading {
  margin-bottom: 14px;
}

.route-card {
  padding: 18px;
  border: 1px solid #34392f;
  background: #11140f;
}

.route-card + .route-card {
  margin-top: 12px;
}

.route-card-heading {
  margin-bottom: 16px;
}

.route-card-heading strong {
  font-size: 0.8rem;
}

.empty-list {
  color: #81897a;
  font-size: 0.78rem;
}

.primary-button,
.review-button,
.secondary-button,
.danger-button,
.add-row,
.danger-link,
.close-button {
  min-height: 41px;
  padding: 10px 14px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font-weight: 700;
}

.primary-button,
.review-button {
  border-color: #b6ff51;
  color: #11150c;
  background: #b6ff51;
}

.review-button {
  margin-left: auto;
  border-color: #b8a6ff;
  background: #b8a6ff;
}

.danger-button,
.danger-link {
  border-color: #75483f;
  color: #ff9b88;
}

.danger-link,
.close-button {
  padding: 6px;
  border: 0;
  background: transparent;
}

button:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

.editor-actions {
  position: sticky;
  bottom: 0;
  z-index: 2;
  justify-content: flex-start;
  padding: 15px clamp(22px, 4vw, 42px);
  border-top: 1px solid #3a4034;
  background: rgb(16 19 14 / 96%);
  backdrop-filter: blur(8px);
}

.diagnostics-section,
.validation-output {
  margin-top: 22px;
}

.output-heading {
  align-items: flex-end;
  margin-bottom: 14px;
}

.output-count {
  color: #808879;
  font-size: 0.72rem;
}

.output-empty {
  padding: 20px;
  border: 1px solid #34392f;
  color: #8a9282;
  background: #11140f;
}

.diagnostic-list {
  margin: 0;
  padding: 0;
  border: 1px solid #34392f;
  list-style: none;
}

.diagnostic {
  display: grid;
  grid-template-columns: 150px minmax(0, 1fr);
  gap: 20px;
  padding: 18px;
  border-left: 3px solid #7b8375;
  background: #11140f;
}

.diagnostic + .diagnostic {
  border-top: 1px solid #34392f;
}

.diagnostic.severity-error {
  border-left-color: #ff745c;
}

.diagnostic.severity-warning {
  border-left-color: #ffbf4b;
}

.diagnostic-code {
  display: grid;
  align-content: start;
  gap: 7px;
}

.diagnostic-code span,
.diagnostic-meta {
  color: #8d9585;
  font-size: 0.66rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.diagnostic p {
  margin: 6px 0 0;
  color: #929a89;
  font-size: 0.76rem;
  line-height: 1.5;
}

.diagnostic-target {
  padding: 0;
  border: 0;
  color: inherit;
  text-align: left;
  background: transparent;
  cursor: pointer;
}

.diagnostic-target:focus-visible {
  outline: 2px solid #b7f34a;
  outline-offset: 4px;
}

.diagnostic .resolution {
  color: #c5d5b2;
}

.validation-output {
  display: grid;
  grid-template-columns: minmax(0, 1.35fr) minmax(300px, 0.65fr);
  gap: 16px;
}

.preview-panel,
.candidate-topology {
  min-width: 0;
  padding: 20px;
  border: 1px solid #34392f;
  background: #11140f;
}

.preview-panel pre {
  max-height: 520px;
  margin: 0;
  padding: 18px;
  overflow: auto;
  color: #d5e5c3;
  background: #090b08;
  font: 0.72rem/1.65 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  white-space: pre;
}

.candidate-node-list {
  display: grid;
  max-height: 520px;
  gap: 1px;
  margin: 0;
  padding: 0;
  overflow: auto;
  background: #30362d;
  list-style: none;
}

.candidate-node-list li {
  display: grid;
  gap: 4px;
  padding: 12px;
  background: #0d100c;
}

.candidate-node-list span,
.candidate-node-list code {
  color: #7f8778;
  font-size: 0.67rem;
}

.candidate-node-list span {
  text-transform: uppercase;
}

.review-scrim {
  position: fixed;
  z-index: 20;
  display: grid;
  inset: 0;
  padding: 18px;
  overflow-y: auto;
  place-items: center;
  background: rgb(5 7 4 / 82%);
  backdrop-filter: blur(5px);
}

.save-review {
  width: min(620px, 100%);
  padding: clamp(22px, 5vw, 38px);
  border: 1px solid #657155;
  background: #151912;
  box-shadow: 0 30px 100px #000;
}

.review-heading {
  align-items: flex-start;
  padding-bottom: 18px;
  border-bottom: 1px solid #34392f;
}

.review-facts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 1px;
  margin: 20px 0;
  background: #34392f;
}

.review-facts div {
  display: grid;
  gap: 7px;
  padding: 14px;
  background: #0e110d;
}

.review-facts dd {
  margin: 0;
  color: #d5dbce;
  font-size: 0.78rem;
}

.review-warning {
  padding: 13px;
  border-left: 3px solid #ffbf4b;
  color: #bdc4b6;
  background: #1c1c15;
  font-size: 0.8rem;
  line-height: 1.55;
}

.dialog-error {
  margin: 18px 0 0;
  padding: 13px;
  border-left: 3px solid #ff745c;
  background: #241410;
  font-size: 0.8rem;
  line-height: 1.55;
}

.review-actions {
  justify-content: flex-end;
  margin-top: 24px;
}

@keyframes config-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 900px) {
  .config-layout {
    grid-template-columns: 210px minmax(0, 1fr);
  }

  .field-grid,
  .validation-output {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .form-heading,
  .revision-banner {
    align-items: flex-start;
    flex-direction: column;
  }

  .revision-board {
    grid-template-columns: 1fr;
  }

  .revision-cell + .revision-cell {
    border-top: 1px solid #34392f;
    border-left: 0;
  }

  .config-layout {
    display: block;
    min-height: 0;
  }

  .object-rail {
    padding: 16px;
    border-right: 0;
    border-bottom: 1px solid #34392f;
  }

  .object-navigation {
    display: none;
  }

  .mobile-nav-label {
    display: block;
    margin-bottom: 7px;
    color: #929a88;
    font-size: 0.7rem;
  }

  .mobile-object-nav {
    display: block;
  }

  .mobile-add-controls {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
    margin-top: 12px;
  }

  .editor-actions {
    position: static;
    align-items: stretch;
    flex-direction: column;
  }

  .review-button {
    margin-left: 0;
  }

  .diagnostic {
    grid-template-columns: 1fr;
    gap: 10px;
  }

  .review-facts {
    grid-template-columns: 1fr;
  }

  .review-actions {
    align-items: stretch;
    flex-direction: column-reverse;
  }

  .primary-button,
  .review-button,
  .secondary-button,
  .danger-button,
  .add-row,
  .danger-link,
  .close-button,
  .mobile-object-nav {
    min-height: 44px;
  }
}

@media (max-width: 440px) {
  .loading-panel,
  .unlock-panel,
  .capability-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .editor-form,
  .object-block,
  .route-card {
    padding-inline: 15px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
