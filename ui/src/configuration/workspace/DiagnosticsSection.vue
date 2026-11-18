<template lang="pug">
section.diagnostics-section(:inert="reviewOpen || undefined" aria-labelledby="diagnostics-heading")
  header.output-heading
    div
      p.eyebrow Server analysis
      h3#diagnostics-heading(tabindex="-1") Validation diagnostics
    span.output-count {{ diagnostics.length }} {{ diagnostics.length === 1 ? 'item' : 'items' }}
  p.output-empty(v-if="diagnostics.length === 0") No diagnostics reported for the current {{ context }}.
  ol.diagnostic-list(v-else aria-live="polite")
    li.diagnostic(v-for="(diagnostic, index) in diagnostics" :key="`${diagnostic.code}-${index}`" :class="`severity-${diagnostic.severity}`")
      .diagnostic-code
        span {{ diagnostic.severity }}
        code {{ diagnostic.code }}
      button.diagnostic-target(v-if="diagnostic.path" type="button" @click="emit('focusField', diagnostic.path)")
        strong {{ diagnostic.message }}
        p.diagnostic-meta {{ diagnostic.stage }}{{ diagnostic.path ? ` / ${diagnostic.path}` : '' }}
      div(v-else)
        strong {{ diagnostic.message }}
        p.diagnostic-meta {{ diagnostic.stage }}
</template>

<script setup lang="ts" vapor>
import type { ConfigDiagnostic } from '../../config'

defineProps<{ diagnostics: ConfigDiagnostic[]; context: string; reviewOpen: boolean }>()
const emit = defineEmits<{ focusField: [path: string] }>()
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./diagnostics.css"></style>
<style scoped src="./output.css"></style>
<style scoped>
.output-empty {
  padding: 20px;
  border: 1px solid #34392f;
  color: #8a9282;
  background: #11140f;
}
</style>
