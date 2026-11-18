<template lang="pug">
Shell(title="OxiRoute" skip-label="Skip to content" v-model:theme="theme" @click="onShellClick")
  template(v-slot:header)
    p.ox-view-title {{ viewTitle(activeView) }}
    .ox-header-end(v-if="signedIn")
      FeedStatus(:state="feedState")
      Button(@click="session.signOut") Sign out
  template(v-slot:above)
    Nav(v-if="signedIn" :items="NAV_ITEMS" :current="activeView" label="Primary navigation" @click="onNavigate")

  LoginPanel(v-if="!signedIn" :rejected="session.rejected.value" @submit="session.signIn")

  template(v-else)
    .ox-notices
      Loading(v-if="activeView !== 'configuration' && !monitoring && refreshing" label="Establishing telemetry")
      Notice(v-if="activeView !== 'configuration' && monitoringError && !monitoring" kind="alert")
        Banner(tone="bad")
          strong Monitoring unavailable.
          |  {{ monitoringError }}
      Notice(v-else-if="activeView !== 'configuration' && monitoring && isStale" kind="status")
        Banner(tone="warn")
          strong Retaining the last valid sample.
          |  {{ staleMessage }}
      Notice(v-if="activeView === 'overview' && catalogError" kind="alert")
        Banner(tone="bad")
          strong {{ catalog ? 'Stream inventory refresh failed.' : 'Stream inventory unavailable.' }}
          |  {{ catalogError }}{{ catalog ? ' The last valid inventory remains visible.' : '' }}
      Notice(v-else-if="activeView === 'overview' && !catalog && refreshing" kind="status")
        Banner(tone="info")
          strong Loading stream inventory.
          |  Waiting for the RTMP catalog.
      Notice(v-if="activeView === 'overview' && rtmpStatsError" kind="alert")
        Banner(tone="bad")
          strong RTMP client control unavailable.
          |  {{ rtmpStatsError }}
      Notice(v-if="activeView === 'overview' && topologyError" kind="alert")
        Banner(tone="bad")
          strong {{ topology ? 'Topology refresh failed.' : 'Topology unavailable.' }}
          |  {{ topologyError }}{{ topology ? ' The last valid topology remains visible.' : '' }}
      Notice(v-else-if="activeView === 'overview' && !topology && refreshing" kind="status")
        Banner(tone="info")
          strong Loading topology.
          |  Waiting for the active runtime graph.
      Notice(v-if="activeView === 'overview' && catalog && !catalog.capabilities.live_ingest" kind="status")
        Banner(tone="warn")
          strong RTMP ingestion is not connected.
          |  Configure an RTMP listener to accept publishers; handshakes never create synthetic streams.
      Notice(v-if="activeView === 'overview' && catalog && !catalog.capabilities.manual_recording" kind="status")
        Banner(tone="warn")
          strong Manual recording controls are unavailable.
          |  Continuous recorder state remains visible, but this runtime does not accept start or stop commands.

    Readouts(v-show="activeView !== 'configuration'" :monitoring="monitoring" :refreshing="refreshing" @refresh="refresh")

    HaproxyStatsDashboard(v-if="activeView === 'stats' && monitoring" :monitoring="monitoring")

    CamerasWorkspace(
      v-if="activeView === 'cameras'"
      :token="token"
      :catalog="catalog"
      :stats="rtmpStats"
      :catalog-error="catalogError"
      :stats-error="rtmpStatsError"
      :now-unix-ms="currentUnixMs"
      :refreshing="refreshing"
      :reload="feed.refreshAfterCommand"
      @unauthorized="session.reject"
    )

    OperationsWorkspace(v-if="activeView === 'operations'" :token="token" @unauthorized="session.reject")

    EventsWorkspace(v-if="activeView === 'events'" :token="token" @unauthorized="session.reject")

    AuditWorkspace(v-if="activeView === 'audit'" :token="token" @unauthorized="session.reject")

    CertificatesWorkspace(v-if="activeView === 'certificates'" :token="token" @unauthorized="session.reject")

    ProvenanceWorkspace(v-if="activeView === 'provenance'" :token="token" @unauthorized="session.reject")

    TopologyView(v-if="activeView === 'overview' && topology" :topology="topology")

    MonitoringOverview(v-if="activeView === 'overview' && monitoring" :monitoring="monitoring" :current-unix-ms="currentUnixMs")

    StreamInventory(
      :catalog="catalog"
      :current-unix-ms="currentUnixMs"
      :busy-recorder-id="busyRecorder"
      :visible="activeView === 'overview'"
      @control="controlRecorder"
    )

    RtmpClientPanel(
      v-if="activeView === 'overview'"
      :stats="rtmpStats"
      :busy-session-id="busySessionId"
      @drop="dropRtmpClient"
    )

  KeepAlive(:max="1")
    ConfigurationWorkspace(
      v-if="signedIn && activeView === 'configuration'"
      :key="session.epoch.value"
      :token="token"
      @unauthorized="session.reject"
    )

  template(v-slot:footer)
    ConfirmHost
    Region
    footer.ox-footer
      p OxiRoute management console
</template>

<script setup lang="ts" vapor>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import { useStoredTheme } from '#design/composables/useTheme'
import Loading from '#design/empty/Loading.vue'
import Shell from '#design/layout/Shell.vue'
import Nav from '#design/nav/Nav.vue'
import Region from '#design/toast/Region.vue'

import AuditWorkspace from './AuditWorkspace.vue'
import CamerasWorkspace from './CamerasWorkspace.vue'
import CertificatesWorkspace from './CertificatesWorkspace.vue'
import ConfigurationWorkspace from './ConfigurationWorkspace.vue'
import EventsWorkspace from './EventsWorkspace.vue'
import HaproxyStatsDashboard from './HaproxyStatsDashboard.vue'
import OperationsWorkspace from './OperationsWorkspace.vue'
import MonitoringOverview from './overview/MonitoringOverview.vue'
import StreamInventory from './overview/StreamInventory.vue'
import ProvenanceWorkspace from './ProvenanceWorkspace.vue'
import RtmpClientPanel from './RtmpClientPanel.vue'
import ConfirmHost from './shell/ConfirmHost.vue'
import FeedStatus from './shell/FeedStatus.vue'
import LoginPanel from './shell/LoginPanel.vue'
import Readouts from './shell/Readouts.vue'
import { useDocumentTitle } from './shell/useDocumentTitle'
import { useManagementSession } from './shell/useManagementSession'
import { useRtmpControls } from './shell/useRtmpControls'
import { useTelemetryFeed } from './shell/useTelemetryFeed'
import { documentTitle, NAV_ITEMS, viewFromHash, viewTitle } from './shell/views'
import type { ViewId } from './shell/views'
import './shell/styles.css'
import TopologyView from './TopologyView.vue'

const activeView = ref<ViewId>(viewFromHash(window.location.hash))
const theme = useStoredTheme()
const session = useManagementSession()
const token = session.token
const signedIn = computed(() => token.value !== '')

const feed = useTelemetryFeed({ token, view: activeView, onUnauthorized: session.reject })
const {
  monitoring, catalog, rtmpStats, topology, monitoringError, catalogError, rtmpStatsError, topologyError,
  refreshing, currentUnixMs, isStale, feedState, staleMessage, refresh,
} = feed
const { busyRecorder, busySessionId, controlRecorder, dropRtmpClient } = useRtmpControls({
  token,
  catalog,
  catalogError,
  rtmpStatsError,
  refreshAfterCommand: feed.refreshAfterCommand,
  onUnauthorized: session.reject,
})

useDocumentTitle(() => documentTitle(activeView.value))

function syncViewFromHash(): void {
  activeView.value = viewFromHash(window.location.hash)
}

const focusMain = () => document.getElementById('ox-main')?.focus()

function onNavigate(event: MouseEvent): void {
  const href = (event.target as Element).closest('a')?.getAttribute('href')
  if (!href) return
  activeView.value = viewFromHash(href)
  focusMain()
}

function onShellClick(event: MouseEvent): void {
  if (!(event.target as Element).closest('.ox-skip')) return
  event.preventDefault()
  focusMain()
}

onMounted(() => window.addEventListener('hashchange', syncViewFromHash))
onUnmounted(() => window.removeEventListener('hashchange', syncViewFromHash))
</script>
