<template lang="pug">
section.validation-output(aria-label="Validated candidate output")
  Card.cfg-preview-panel
    header.output-heading
      div
        p.cfg-eyebrow Backend rendered
        h3 {{ formatLabel(result.configFormat) }} configuration preview
      code {{ shortRevision(result.candidateRevision) }}
    pre(tabindex="0") {{ result.configPreview }}
  Card.candidate-topology
    header.output-heading
      div
        p.cfg-eyebrow Candidate graph
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
import Card from '#design/panel/Card.vue'
import type { ConfigValidationResponse } from '../../api'
import { shortRevision } from '../../formatters'
import { formatLabel } from './configFormat'

defineProps<{ result: ConfigValidationResponse }>()
</script>
