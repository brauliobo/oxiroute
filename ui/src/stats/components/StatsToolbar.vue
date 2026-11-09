<script setup vapor lang="ts">
import { DEFAULT_INTERVAL_MS } from '../poll'
import { useStatsStore } from '../useStatsStore'
import FilterChips from './FilterChips.vue'
import IntervalSelect from './IntervalSelect.vue'
import PauseControl from './PauseControl.vue'
import SearchBox from './SearchBox.vue'
import SortMenu from './SortMenu.vue'
import ThemeToggle from './ThemeToggle.vue'

const { view, snapshot, setView, togglePause, setIntervalMs, refresh } = useStatsStore()
</script>

<template lang="pug">
section.ox-toolbar(role="search" aria-label="Filter and view")
  SearchBox(:model-value="view.search" @update:modelValue="(search) => setView({ search })")
  FilterChips(
    :states="view.states"
    :problems-only="view.problemsOnly"
    @update:states="(states) => setView({ states })"
    @update:problemsOnly="(problemsOnly) => setView({ problemsOnly })"
  )
  SortMenu(:sort="view.sort" @update:sort="(sort) => setView({ sort })")
  PauseControl(:paused="view.paused" @toggle="togglePause" @refresh="refresh")
  IntervalSelect(
    :model-value="view.intervalMs"
    :hint-ms="snapshot?.intervalHintMs ?? DEFAULT_INTERVAL_MS"
    @update:modelValue="setIntervalMs"
  )
  ThemeToggle(:model-value="view.theme" @update:modelValue="(theme) => setView({ theme })")
</template>
