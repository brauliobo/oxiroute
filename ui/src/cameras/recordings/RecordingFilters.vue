<script setup vapor lang="ts">
import { computed, useId } from 'vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import { STATUS_OPTIONS, valueOptions, type FilterName, type Filters } from './filters'
import { parseSort, SORT_OPTIONS, sortValue, type SortState } from './sort'
import './page.css'

const props = defineProps<{
  filters: Filters
  sort: SortState
  streams: readonly string[]
  codecs: readonly string[]
  active: boolean
}>()
const emit = defineEmits<{ change: [name: FilterName, value: string]; sort: [state: SortState]; clear: [] }>()

const ids = { stream: useId(), status: useId(), from: useId(), to: useId(), codec: useId(), search: useId(), sort: useId() }

const streamOptions = computed(() => valueOptions('All streams', props.streams, props.filters.stream))
const codecOptions = computed(() => valueOptions('All codecs', props.codecs, props.filters.codec))

const pickSort = (value: string) => {
  const state = parseSort(value)
  if (state) emit('sort', state)
}
</script>

<template lang="pug">
form.ox-recs-filters(role="search" aria-label="Filter recordings" @submit.prevent)
  Field(stacked label="Stream" :label-for="ids.stream")
    Select(:id="ids.stream" :model-value="filters.stream" :options="streamOptions" @update:model-value="(value) => emit('change', 'stream', value)")
  Field(stacked label="Status" :label-for="ids.status")
    Select(:id="ids.status" :model-value="filters.status" :options="STATUS_OPTIONS" @update:model-value="(value) => emit('change', 'status', value)")
  Field(stacked label="From (UTC day)" :label-for="ids.from")
    Input(:id="ids.from" type="date" :model-value="filters.from" @update:model-value="(value) => emit('change', 'from', value)")
  Field(stacked label="To (UTC day, inclusive)" :label-for="ids.to")
    Input(:id="ids.to" type="date" :model-value="filters.to" @update:model-value="(value) => emit('change', 'to', value)")
  Field(stacked label="Video codec" :label-for="ids.codec")
    Select(:id="ids.codec" :model-value="filters.codec" :options="codecOptions" @update:model-value="(value) => emit('change', 'codec', value)")
  Field(stacked label="Search by name" :label-for="ids.search")
    Input(:id="ids.search" type="search" autocomplete="off" :model-value="filters.search" @update:model-value="(value) => emit('change', 'search', value)")
  Field.ox-recs-sort-pick(stacked label="Sort by" :label-for="ids.sort")
    Select(:id="ids.sort" :model-value="sortValue(sort)" :options="SORT_OPTIONS" @update:model-value="pickSort")
  .ox-recs-filter-actions
    Button(data-action="clear-filters" :disabled="!active" @click="emit('clear')") Clear filters
</template>
