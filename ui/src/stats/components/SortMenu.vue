<script setup vapor lang="ts">
import { SORT_COLUMNS } from '../query'
import type { SortColumn } from '../query'
import type { SortSpec } from '../urlState'

const props = defineProps<{ sort: SortSpec | undefined }>()
const emit = defineEmits<{ 'update:sort': [sort: SortSpec | undefined] }>()

const LABELS: Readonly<Record<SortColumn, string>> = {
  name: 'Name', status: 'Status', active: 'Active', rate: 'Rate', errors: 'Errors', p99: 'p99', received: 'Received', sent: 'Sent',
}

const choose = (event: Event) => {
  const column = (event.target as HTMLSelectElement).value
  emit('update:sort', column ? { column, direction: props.sort?.direction ?? 'asc' } : undefined)
}
const flip = () => props.sort && emit('update:sort', { ...props.sort, direction: props.sort.direction === 'asc' ? 'desc' : 'asc' })
</script>

<template lang="pug">
.ox-field
  label(for="ox-sort") Sort by
  select#ox-sort.ox-input(name="sort" @change="choose")
    option(value="" :selected="!sort") Default order
    option(v-for="column in SORT_COLUMNS" :key="column" :value="column" :selected="sort?.column === column") {{ LABELS[column] }}
  button.ox-btn(type="button" :disabled="!sort" :aria-pressed="sort?.direction === 'desc' ? 'true' : 'false'" @click="flip") Descending
</template>
