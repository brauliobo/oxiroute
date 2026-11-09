<script setup vapor lang="ts">
import { computed } from 'vue'
import type { SortColumn } from '../query'
import { useStatsStore } from '../useStatsStore'
import { ariaSort, nextSort } from './sorting'
import './table.css'

// A column header; with a `column` it is a button that sorts the table through the store's view (also in the URL).
const props = defineProps<{ label: string; column?: SortColumn }>()
const { view, setView } = useStatsStore()
const sorted = computed(() => (props.column ? ariaSort(view.value.sort, props.column) : undefined))
const mark = computed(() => ({ ascending: '▲', descending: '▼', none: '' })[sorted.value ?? 'none'])
const sort = () => setView({ sort: nextSort(view.value.sort, props.column!) })
</script>

<template lang="pug">
th(scope="col" :aria-sort="sorted")
  button.ox-sort(v-if="column" type="button" @click="sort")
    | {{ label }}
    span.ox-sort-mark(aria-hidden="true") {{ mark }}
  template(v-else) {{ label }}
</template>
