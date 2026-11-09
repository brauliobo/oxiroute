<script setup vapor lang="ts">
import { computed, onMounted, onScopeDispose, ref, watchEffect } from 'vue'
import AdminActions from './components/AdminActions.vue'
import Footer from './components/Footer.vue'
import ShortcutsDialog from './components/ShortcutsDialog.vue'
import StatsDashboard from './components/StatsDashboard.vue'
import StatsToolbar from './components/StatsToolbar.vue'
import StatusBanner from './components/StatusBanner.vue'
import { statusBanner } from './banner'
import { csvUrl, jsonUrl } from './links'
import { SEARCH_ID, shortcutAction } from './shortcuts'
import type { ShortcutAction } from './shortcuts'
import StatsStatus from './StatsStatus.vue'
import { applyTheme, nextTheme } from './theme'
import { provideStatsStore } from './useStatsStore'

const { view, status, failure, model, snapshot, setView, togglePause, refresh } = provideStatsStore()
const csvHref = csvUrl(location.pathname)
const jsonHref = jsonUrl(location.pathname)
const helpOpen = ref(false)
const banner = computed(() => statusBanner(status.value, failure.value))
const revision = computed(() => model.value?.generation.wire.activeRevision)

watchEffect(() => applyTheme(view.value.theme))

const SHORTCUT_HANDLERS: Readonly<Record<ShortcutAction, () => void>> = {
  search: () => document.getElementById(SEARCH_ID)?.focus(),
  help: () => { helpOpen.value = true },
  pause: togglePause,
  refresh,
  theme: () => setView({ theme: nextTheme(view.value.theme) }),
  problems: () => setView({ problemsOnly: !view.value.problemsOnly }),
}
function onKeydown(event: KeyboardEvent) {
  const action = shortcutAction(event)
  if (!action) return
  event.preventDefault()
  SHORTCUT_HANDLERS[action]()
}
onMounted(() => document.addEventListener('keydown', onKeydown))
onScopeDispose(() => document.removeEventListener('keydown', onKeydown))
</script>

<template lang="pug">
.ox-app
  a.ox-skip(href="#ox-main") Skip to statistics
  header.ox-header
    h1 OxiRoute statistics
    StatsStatus
    nav.ox-exports(aria-label="Export")
      a(:href="csvHref" download="stats.csv") CSV
      a(:href="jsonHref") JSON
  StatsToolbar
  StatusBanner(:banner="banner" @action="(action) => (action === 'retry' ? refresh() : location.reload())")
  AdminActions(v-if="model?.admin && revision" :pools="model.pools" :revision="revision" @changed="refresh")
  main#ox-main(tabindex="-1")
    StatsDashboard
  Footer(
    :version="snapshot?.process.version"
    :generation="snapshot?.generation.number"
    :sampled-at-unix-ms="snapshot?.sampledAtUnixMs"
    @help="helpOpen = true"
  )
  ShortcutsDialog(:open="helpOpen" @close="helpOpen = false")
</template>
