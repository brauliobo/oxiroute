<script setup vapor lang="ts">
import { computed, nextTick, ref } from 'vue'
import Button from '#design/button/Button.vue'
import Panel from '#design/panel/Panel.vue'
import StatusBadge from '#design/status/StatusBadge.vue'
import { vCssVars } from '#design/directives/cssVars'
import type { TopologySnapshot } from './api'
import GraphNode from './topology/GraphNode.vue'
import Inspector from './topology/Inspector.vue'
import { nextFocusIndex } from './topology/focus'
import { edgeLabels } from './topology/labels'
import { NODE_HEIGHT, NODE_WIDTH, layoutTopology } from './topology/layout'
import './topology/styles.css'

const props = defineProps<{ topology: TopologySnapshot }>()

const selectedNodeId = ref<string | null>(null)
const layout = computed(() => layoutTopology(props.topology.nodes, props.topology.edges))
const overlays = computed(() => new Map(props.topology.overlays.map((overlay) => [overlay.nodeId, overlay])))
const selectedNode = computed(() => props.topology.nodes.find((node) => node.id === selectedNodeId.value) ?? null)
const stageVars = computed(() => ({
  '--stage-width': `${layout.value.width}px`,
  '--stage-height': `${layout.value.height}px`,
  '--node-w': `${NODE_WIDTH}px`,
  '--node-h': `${NODE_HEIGHT}px`,
}))
const viewBox = computed(() => `0 0 ${layout.value.width} ${layout.value.height}`)

function nodeVars(nodeId: string): Record<string, string> {
  const position = layout.value.positions.get(nodeId)
  return position ? { '--node-x': `${position.x}px`, '--node-y': `${position.y}px` } : {}
}

const nodeName = (nodeId: string): string => props.topology.nodes.find((node) => node.id === nodeId)?.name ?? 'Unknown node'
const nodeButtons = (): HTMLElement[] => Array.from(document.querySelectorAll<HTMLElement>('[data-node-id]'))

async function selectNode(nodeId: string): Promise<void> {
  selectedNodeId.value = nodeId
  await nextTick()
  document.querySelector('.ox-inspector')?.scrollIntoView({ block: 'nearest' })
}

async function closeInspector(): Promise<void> {
  const nodeId = selectedNodeId.value
  if (!nodeId) return
  selectedNodeId.value = null
  await nextTick()
  nodeButtons().find((button) => button.dataset.nodeId === nodeId)?.focus()
}

function moveFocus(event: KeyboardEvent): void {
  const buttons = nodeButtons()
  const current = buttons.findIndex((button) => button.contains(event.target as Node))
  const target = nextFocusIndex(event.key, current, buttons.length)
  if (target === null) return
  event.preventDefault()
  buttons[target]!.focus()
}
</script>

<template lang="pug">
Panel.ox-view-section(data-section="topology" title="Network topology" @keydown.esc="closeInspector")
  .ox-topology-head
    p.ox-muted Listener-to-origin dispatch from the validated runtime generation.
    p.ox-topology-state(role="status")
      StatusBadge(:status="topology.state.runtime")
      span schema {{ topology.schemaVersion }}
  .ox-topology
    .ox-topology-graph
      .ox-topology-viewport(role="group" aria-label="Active network topology")
        .ox-topology-stage(v-css-vars="stageVars" @keydown="moveFocus")
          svg.ox-connectors(aria-hidden="true" focusable="false" :viewBox="viewBox" preserveAspectRatio="none")
            defs
              marker#ox-topology-arrow(markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto" markerUnits="strokeWidth")
                path(d="M0,0 L8,4 L0,8 Z")
            path.ox-connector(
              v-for="connector in layout.connectors"
              :key="connector.id"
              :class="{ 'ox-connector-tls': connector.tls }"
              :d="connector.path"
              marker-end="url(#ox-topology-arrow)"
            )
          GraphNode(
            v-for="node in layout.orderedNodes"
            :key="node.id"
            :node="node"
            :overlay="overlays.get(node.id)"
            :selected="node.id === selectedNodeId"
            :vars="nodeVars(node.id)"
            @select="selectNode(node.id)"
          )
      section.ox-relations(aria-labelledby="ox-relations-title")
        h3#ox-relations-title Topology relations
        p.ox-muted(v-if="topology.edges.length === 0") No configured relations.
        ul(v-else)
          li(v-for="edge in topology.edges" :key="edge.id")
            Button.ox-relation(@click="selectNode(edge.target)")
              strong {{ nodeName(edge.source) }}
              span {{ edgeLabels[edge.kind] }}
              strong {{ nodeName(edge.target) }}
    Inspector(
      v-if="selectedNode"
      :node="selectedNode"
      :overlay="overlays.get(selectedNode.id)"
      @close="closeInspector"
    )
    p.ox-muted(v-else) Select any node to inspect its redacted attributes, runtime state and stable config path.
</template>
