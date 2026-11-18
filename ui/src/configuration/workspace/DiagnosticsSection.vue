<template lang="pug">
section.diagnostics-section(aria-labelledby="diagnostics-heading")
  header.output-heading
    div
      p.eyebrow Server analysis
      h3#diagnostics-heading(tabindex="-1") Validation diagnostics
    span.output-count {{ summary }}
  EmptyState.output-empty(v-if="placed.length === 0") No diagnostics reported for the current {{ context }}.
  ol.diagnostic-list(v-else)
    li.diagnostic(v-for="item in placed" :key="item.id" :class="`severity-${item.diagnostic.severity}`")
      .diagnostic-code
        span {{ item.diagnostic.severity }}
        code {{ item.diagnostic.code }}
      button.diagnostic-target(v-if="item.location" type="button" @click="emit('focusField', item)")
        DiagnosticText(:diagnostic="item.diagnostic")
      div(v-else)
        DiagnosticText(:diagnostic="item.diagnostic")
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import EmptyState from '#design/empty/EmptyState.vue'

import type { PlacedDiagnostic } from '../fields/context'
import DiagnosticText from './DiagnosticText.vue'
import { summarizeDiagnostics } from './diagnosticSummary'

const props = defineProps<{ placed: PlacedDiagnostic[]; context: string }>()
const emit = defineEmits<{ focusField: [item: PlacedDiagnostic] }>()
const summary = computed(() => summarizeDiagnostics(props.placed.map(({ diagnostic }) => diagnostic)))
</script>
