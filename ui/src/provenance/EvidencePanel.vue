<script setup vapor lang="ts">
import EmptyState from '#design/empty/EmptyState.vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import ScrollRegion from '../workspace/ScrollRegion.vue'

defineProps<{
  title: string
  note: string
  caption: string
  columns: readonly string[]
  rows: readonly (readonly string[])[]
  empty: string
}>()
</script>

<template lang="pug">
Panel(:title="title")
  .ox-stack
    p.ox-muted {{ note }}
    EmptyState(v-if="rows.length === 0") {{ empty }}
    ScrollRegion(v-else :label="`${caption} table`")
      Table(:caption="caption" :columns="columns")
        tbody
          tr(v-for="(row, index) in rows" :key="index")
            th.ox-name(scope="row") {{ row[0] }}
            td(v-for="(cell, column) in row.slice(1)" :key="column" :data-label="columns[column + 1]") {{ cell }}
</template>
