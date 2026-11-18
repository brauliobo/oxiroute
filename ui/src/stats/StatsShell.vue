<script setup vapor lang="ts">
import { computed, ref } from 'vue'
import { nextTheme, useTheme } from '#design/composables/useTheme'
import { useShortcuts } from '#design/composables/useShortcuts'
import Shell from '#design/layout/Shell.vue'
import AdminActions from './components/AdminActions.vue'
import Footer from './components/Footer.vue'
import ShortcutsDialog from './components/ShortcutsDialog.vue'
import StatsDashboard from './components/StatsDashboard.vue'
import StatsToolbar from './components/StatsToolbar.vue'
import StatusBanner from './components/StatusBanner.vue'
import { statusBanner } from './banner'
import { csvUrl, jsonUrl } from './links'
import { KEYMAP, SEARCH_ID } from './shortcuts'
import type { ShortcutAction } from './shortcuts'
import StatsStatus from './StatsStatus.vue'
import { provideStatsStore } from './useStatsStore'

const { view, status, failure, model, snapshot, setView, togglePause, refresh } = provideStatsStore()
const csvHref = csvUrl(location.pathname)
const jsonHref = jsonUrl(location.pathname)
const helpOpen = ref(false)
const banner = computed(() => statusBanner(status.value, failure.value))
const revision = computed(() => model.value?.generation.wire.activeRevision)

useTheme(computed(() => view.value.theme))

function onBannerAction(action: 'retry' | 'reload'): void {
  if (action === 'retry') refresh()
  else location.reload()
}

const SHORTCUT_HANDLERS: Readonly<Record<ShortcutAction, () => void>> = {
  search: () => document.getElementById(SEARCH_ID)?.focus(),
  help: () => { helpOpen.value = true },
  pause: togglePause,
  refresh,
  theme: () => setView({ theme: nextTheme(view.value.theme) }),
  problems: () => setView({ problemsOnly: !view.value.problemsOnly }),
}
useShortcuts(KEYMAP, SHORTCUT_HANDLERS)
</script>

<template lang="pug">
Shell(title="OxiRoute statistics" skip-label="Skip to statistics")
  template(v-slot:header)
    StatsStatus
    nav.ox-exports(aria-label="Export")
      a(:href="csvHref" download="stats.csv") CSV
      a(:href="jsonHref") JSON
  template(v-slot:above)
    StatsToolbar
    StatusBanner(:banner="banner" @action="onBannerAction")
    AdminActions(v-if="model?.admin && revision" :pools="model.pools" :revision="revision" @changed="refresh")
  StatsDashboard
  template(v-slot:footer)
    Footer(
      :version="snapshot?.process.version"
      :generation="snapshot?.generation.number"
      :sampled-at-unix-ms="snapshot?.sampledAtUnixMs"
      @help="helpOpen = true"
    )
    ShortcutsDialog(:open="helpOpen" @close="helpOpen = false")
</template>
