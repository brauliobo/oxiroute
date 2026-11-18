<script setup vapor lang="ts">
import SortHeader from '#design/table/SortHeader.vue'
import type { SortColumn } from '../query'
import { useStatsStore } from '../useStatsStore'
import type { Column } from './columns'
import { ariaSort, nextSort } from './sorting'

defineProps<{ columns: readonly Column[] }>()
const { view, setView } = useStatsStore()

const sort = (column: SortColumn | undefined) => column && setView({ sort: nextSort(view.value.sort, column) })
const direction = (column: SortColumn | undefined) => (column ? ariaSort(view.value.sort, column) : undefined)
</script>

<template lang="pug">
tr
  SortHeader(
    v-for="item in columns"
    :key="item.label"
    :label="item.label"
    :sortable="item.column !== undefined"
    :direction="direction(item.column)"
    @sort="sort(item.column)"
  )
</template>
