<script setup vapor lang="ts">
import Chip from '#design/button/Chip.vue'
import { STATUS_LABELS } from '../model'
import { SERVER_STATUSES } from '../urlState'
import type { StatusFilter } from '../urlState'

const props = defineProps<{ states: StatusFilter[]; problemsOnly: boolean }>()
const emit = defineEmits<{ 'update:states': [states: StatusFilter[]]; 'update:problemsOnly': [problemsOnly: boolean] }>()

const toggle = (status: StatusFilter) =>
  emit('update:states', SERVER_STATUSES.filter((item) => (item === status) !== props.states.includes(item)))
</script>

<template lang="pug">
.ox-chips(role="group" aria-labelledby="ox-chips-label")
  span#ox-chips-label.ox-chips-label Server state
  Chip(v-for="status in SERVER_STATUSES" :key="status" :pressed="props.states.includes(status)" @click="toggle(status)") {{ STATUS_LABELS[status] }}
Chip.ox-chip-problems(:pressed="problemsOnly" @click="emit('update:problemsOnly', !problemsOnly)") Problems only
</template>
