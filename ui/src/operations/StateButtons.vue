<script setup vapor lang="ts">
import Button from '#design/button/Button.vue'
import type { AdministrativeState } from '../api'
import { ADMIN_STATES } from './labels'
import './page.css'

const ACTIONS: Record<AdministrativeState, string> = { ready: 'Ready', drain: 'Drain', maintenance: 'Maintenance' }

defineProps<{ current: AdministrativeState | null; subject: string; disabled: boolean }>()
const emit = defineEmits<{ change: [state: AdministrativeState] }>()
</script>

<template lang="pug">
.ox-ops-buttons(role="group" :aria-label="`Administrative state of ${subject}`")
  Button(
    v-for="state in ADMIN_STATES"
    :key="state"
    small
    :pressed="current === state"
    :disabled="disabled"
    :data-state="state"
    :aria-label="`${ACTIONS[state]} ${subject}`"
    @click="emit('change', state)"
  ) {{ ACTIONS[state] }}
</template>
