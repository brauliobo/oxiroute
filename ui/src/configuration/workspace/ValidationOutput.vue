<template lang="pug">
section.validation-output(:inert="reviewOpen ? '' : null" aria-label="Validated candidate output")
  article.preview-panel
    header.output-heading
      div
        p.eyebrow Backend rendered
        h3 {{ formatLabel(result.configFormat) }} configuration preview
      code {{ shortRevision(result.candidateRevision) }}
    pre(tabindex="0") {{ result.configPreview }}
  article.candidate-topology
    header.output-heading
      div
        p.eyebrow Candidate graph
        h3 Validation topology
      span.output-count {{ result.topology.nodes.length }} nodes / {{ result.topology.edges.length }} edges
    p.candidate-state Candidate only / not active
    ul.candidate-node-list
      li(v-for="node in result.topology.nodes" :key="node.id")
        span {{ node.kind.replaceAll('_', ' ') }}
        strong {{ node.name }}
        code {{ node.configPath }}
</template>

<script setup lang="ts" vapor>
import type { ConfigValidationResponse } from '../../api'
import { shortRevision } from '../../formatters'
import { formatLabel } from './configFormat'

defineProps<{ result: ConfigValidationResponse; reviewOpen: boolean }>()
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./output.css"></style>
<style scoped>
.validation-output {
  display: grid;
  grid-template-columns: minmax(0, 1.35fr) minmax(300px, 0.65fr);
  gap: 16px;
}

.preview-panel,
.candidate-topology {
  min-width: 0;
  padding: 20px;
  border: 1px solid #34392f;
  background: #11140f;
}

.preview-panel pre {
  max-height: 520px;
  margin: 0;
  padding: 18px;
  overflow: auto;
  color: #d5e5c3;
  background: #090b08;
  font: 0.72rem/1.65 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  white-space: pre;
}

.candidate-node-list {
  display: grid;
  max-height: 520px;
  gap: 1px;
  margin: 0;
  padding: 0;
  overflow: auto;
  background: #30362d;
  list-style: none;
}

.candidate-node-list li {
  display: grid;
  gap: 4px;
  padding: 12px;
  background: #0d100c;
}

.candidate-node-list span,
.candidate-node-list code {
  color: #7f8778;
  font-size: 0.67rem;
}

.candidate-node-list span {
  text-transform: uppercase;
}

@media (max-width: 900px) {
  .validation-output {
    grid-template-columns: 1fr;
  }
}
</style>
