<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import Card from '#design/panel/Card.vue'
import { WEIGHT_RANGE, type AdministrativeState, type HealthOverride, type ServerInventoryEntry } from '../api'
import { formatCount } from '../formatters'
import type { ServerCommand } from './commands'
import { ADMIN_BADGES, HEALTH_BADGES, checksApplicable, humanize, isDnsAddress, limitText } from './labels'
import LimitForm from './LimitForm.vue'
import StateButtons from './StateButtons.vue'
import './page.css'

const HEALTH_CHOICES: ReadonlyArray<{ health: HealthOverride; label: string }> = [
  { health: 'auto', label: 'Auto health' },
  { health: 'up', label: 'Force up' },
  { health: 'down', label: 'Force down' },
]

const props = defineProps<{ entry: ServerInventoryEntry; disabled: boolean }>()
const emit = defineEmits<{ command: [command: ServerCommand] }>()

const server = computed(() => props.entry.server)
const subject = computed(() => `server ${server.value.name} in pool ${props.entry.pool}`)
const checked = computed(() => checksApplicable(server.value))
const dns = computed(() => isDnsAddress(server.value.address))

const setState = (state: AdministrativeState) => emit('command', { kind: 'state', entry: props.entry, state })
const setHealth = (health: HealthOverride) => emit('command', { kind: 'health', entry: props.entry, health })
const toggleChecks = () => emit('command', { kind: 'checks', entry: props.entry })
const refreshDns = () => emit('command', { kind: 'dns', entry: props.entry })
const evacuate = () => emit('command', { kind: 'evacuate', entry: props.entry })
const setLimit = (limit: number | null) => emit('command', { kind: 'limit', entry: props.entry, limit })
const setWeight = (weight: number | null) => emit('command', { kind: 'weight', entry: props.entry, weight })
</script>

<template lang="pug">
Card.ox-ops-server(:data-server="server.name" :data-pool="entry.pool")
  header.ox-ops-server-head
    div
      h3 {{ server.name }}
      p.ox-muted {{ entry.pool }} / {{ server.address }} / {{ server.origin }}
    .ox-ops-badges
      Badge(:tone="ADMIN_BADGES[server.administrativeState].tone" :label="ADMIN_BADGES[server.administrativeState].label")
      Badge(:tone="HEALTH_BADGES[server.state].tone" :label="HEALTH_BADGES[server.state].label")
      Badge(v-if="server.passiveEjected" tone="bad" label="Ejected")
  dl.ox-ops-facts
    div
      dt Connections
      dd {{ formatCount(server.activeConnections) }} of {{ limitText(server.maxConnections) }}
    div
      dt Weight
      dd {{ server.weight }}
    div(v-if="server.healthOverride !== 'auto'")
      dt Health override
      dd {{ server.healthOverride === 'up' ? 'Forced up' : 'Forced down' }}
    div(v-if="checked")
      dt Checks
      dd {{ server.checksEnabled ? 'Enabled' : 'Disabled' }}, {{ formatCount(server.successfulChecks) }} passed, {{ formatCount(server.failedChecks) }} failed
    div(v-if="server.lastFailure")
      dt Last failure
      dd {{ humanize(server.lastFailure) }}
  .ox-ops-controls
    StateButtons(:current="server.administrativeState" :subject="subject" :disabled="disabled" @change="setState")
    .ox-ops-buttons(role="group" :aria-label="`Health override of ${subject}`")
      Button(
        v-for="choice in HEALTH_CHOICES"
        :key="choice.health"
        small
        :pressed="server.healthOverride === choice.health"
        :disabled="disabled"
        :data-health="choice.health"
        :aria-label="`${choice.label} ${subject}`"
        @click="setHealth(choice.health)"
      ) {{ choice.label }}
    .ox-ops-buttons
      Button(v-if="checked" small :disabled="disabled" data-action="checks" :aria-label="`${server.checksEnabled ? 'Disable' : 'Enable'} checks of ${subject}`" @click="toggleChecks") {{ server.checksEnabled ? 'Disable checks' : 'Enable checks' }}
      Button(v-if="dns" small :disabled="disabled" data-action="dns" :aria-label="`Refresh DNS of ${subject}`" @click="refreshDns") Refresh DNS
      Button(small variant="danger" :disabled="disabled" data-action="evacuate" :aria-label="`Evacuate ${subject}`" @click="evacuate") Evacuate
  .ox-ops-forms
    LimitForm(
      label="Max connections"
      :subject="subject"
      apply-label="Apply limit"
      reset-label="Reset limit"
      placeholder="unbounded"
      :min="1"
      :max="null"
      :disabled="disabled"
      @apply="setLimit"
      @reset="setLimit(null)"
    )
    LimitForm(
      label="Weight"
      :subject="subject"
      apply-label="Apply weight"
      reset-label="Reset weight"
      :placeholder="String(server.weight)"
      :min="WEIGHT_RANGE.min"
      :max="WEIGHT_RANGE.max"
      :disabled="disabled"
      @apply="setWeight"
      @reset="setWeight(null)"
    )
</template>
