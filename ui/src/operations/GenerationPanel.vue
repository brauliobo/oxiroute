<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Button from '#design/button/Button.vue'
import { formatDuration } from '#design/format'
import Panel from '#design/panel/Panel.vue'
import type { GenerationStatus, RuntimeStatus } from '../api'
import { formatCount, shortRevision } from '../formatters'
import type { GenerationCommand } from './commands'
import { humanize, plural, type StateBadge } from './labels'
import './page.css'

const PENDING_REASONS: Record<string, string> = {
  retired_cap: 'too many replaced generations are still draining',
  retired_draining: 'the previous worker is still draining',
}

const props = defineProps<{ status: RuntimeStatus | null; generation: GenerationStatus | null; disabled: boolean }>()
const emit = defineEmits<{ command: [command: GenerationCommand] }>()

const badge = computed<StateBadge>(() => {
  if (props.status?.degraded || props.generation?.degraded) return { label: 'Degraded', tone: 'bad' }
  if (props.generation?.activeAccepting === false) return { label: 'Draining', tone: 'warn' }
  return { label: 'Ready', tone: 'ok' }
})
const active = computed(() => props.generation?.activeRevision ?? props.status?.activeRevision ?? null)
const disk = computed(() => props.generation?.diskRevision ?? props.status?.diskRevision ?? null)
const previous = computed(() => props.generation?.previousRevision ?? props.status?.previousRevision ?? null)
const pending = computed(() => props.generation?.pendingApply ?? null)
const failure = computed(() => props.generation?.lastFailure ?? (props.status?.degraded ? 'Runtime degraded' : 'None reported'))

const run = (kind: GenerationCommand['kind']) => emit('command', { kind })
</script>

<template lang="pug">
Panel.ox-ops-generation(title="Runtime status and generations")
  .ox-ops-badges
    Badge(:tone="badge.tone" :label="badge.label")
    Badge(v-if="generation" :tone="generation.activeAccepting ? 'ok' : 'warn'" :label="generation.activeAccepting ? 'Accepting new traffic' : 'Not accepting new traffic'")
  dl.ox-ops-facts
    div
      dt Active revision
      dd
        code {{ shortRevision(active) }}
    div
      dt Disk revision
      dd
        code {{ shortRevision(disk) }}
    div
      dt Candidate
      dd
        code {{ shortRevision(generation?.candidateRevision ?? status?.candidateRevision ?? null) }}
    div
      dt Previous (rollback target)
      dd
        code {{ shortRevision(previous) }}
    div(v-if="generation?.quarantinedRevision")
      dt Quarantined
      dd
        code {{ shortRevision(generation.quarantinedRevision) }}
    div
      dt Last failure
      dd {{ failure }}
    div(v-if="generation")
      dt Counters
      dd {{ generation.activations }} activations, {{ generation.failures }} failures, {{ generation.rollbacks }} rollbacks
  Banner.ox-ops-pending(v-if="pending" tone="warn" data-pending-apply)
    strong A reload is waiting.
    |  Revision {{ shortRevision(pending.revision) }} waits because {{ PENDING_REASONS[pending.reason] ?? humanize(pending.reason) }}
    span(v-if="pending.connections !== null") : {{ plural(pending.connections, 'connection') }} remain.
  ul.ox-ops-detail(v-if="generation && generation.retiredGenerations.length")
    li(v-for="retired in generation.retiredGenerations" :key="retired.revision")
      | Draining {{ shortRevision(retired.revision) }}: {{ formatCount(retired.inFlight) }} in flight, {{ formatDuration(retired.deadlineRemainingSeconds * 1000) }} left
  .ox-ops-buttons
    Button(variant="primary" data-generation-action="reload" :disabled="disabled" @click="run('reload')") Reload persisted configuration
    Button(data-generation-action="rollback" :disabled="disabled || !previous" @click="run('rollback')") Roll back to previous
    Button(variant="danger" data-generation-action="drain" :disabled="disabled" @click="run('drain-generation')") Drain active generation
    Button(variant="danger" data-process-action="drain" :disabled="disabled" @click="run('drain-process')") Drain process
    Button(variant="danger" data-process-action="shutdown" :disabled="disabled" @click="run('shutdown')") Request shutdown
</template>
