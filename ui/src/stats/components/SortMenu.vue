<script setup vapor lang="ts">
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'
import { SORT_COLUMNS } from '../query'
import type { SortColumn } from '../query'
import type { SortSpec } from '../urlState'

const props = defineProps<{ sort: SortSpec | undefined }>()
const emit = defineEmits<{ 'update:sort': [sort: SortSpec | undefined] }>()

const LABELS: Readonly<Record<SortColumn, string>> = {
  name: 'Name', status: 'Status', active: 'Active', rate: 'Rate', errors: 'Errors', p99: 'p99', received: 'Received', sent: 'Sent',
}
const options = [{ value: '', label: 'Default order' }, ...SORT_COLUMNS.map((column) => ({ value: column, label: LABELS[column] }))]

const choose = (column: string) => emit('update:sort', column ? { column, direction: props.sort?.direction ?? 'asc' } : undefined)
const flip = () => props.sort && emit('update:sort', { ...props.sort, direction: props.sort.direction === 'asc' ? 'desc' : 'asc' })
</script>

<template lang="pug">
Field(label="Sort by" label-for="ox-sort")
  Select#ox-sort(name="sort" :model-value="sort?.column ?? ''" :options="options" @update:modelValue="choose")
  Button(:disabled="!sort" :pressed="sort?.direction === 'desc'" @click="flip") Descending
</template>
