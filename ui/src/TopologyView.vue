<template lang="pug">
section.topology-section(aria-labelledby="topology-heading" @keydown.esc="closeInspector")
  .topology-heading
    div
      p.eyebrow Active configuration
      h2#topology-heading Network topology
      p.topology-deck Listener-to-origin dispatch from the validated runtime generation.
    .topology-state(role="status")
      span.state-light(aria-hidden="true")
      span Active / schema {{ topology.schemaVersion }}

  .topology-workspace
    .schematic-viewport(aria-label="Active network topology")
      .topology-stage(:style="stageStyle")
        svg.connector-layer(
          aria-hidden="true"
          :viewBox="`0 0 ${CANVAS_WIDTH} ${canvasHeight}`"
          preserveAspectRatio="none"
        )
          defs
            marker#topology-arrow(markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto" markerUnits="strokeWidth")
              path(d="M0,0 L8,4 L0,8 Z")
          path.connector(
            v-for="edge in connectorPaths"
            :key="edge.id"
            :class="`edge-${edge.kind}`"
            :d="edge.path"
            marker-end="url(#topology-arrow)"
          )

        button.topology-node(
          v-for="(node, index) in orderedNodes"
          :key="node.id"
          type="button"
          :data-node-id="node.id"
          :class="[`node-${node.kind}`, `state-${overlayFor(node.id)?.state ?? 'configured'}`, { selected: node.id === selectedNodeId }]"
          :style="nodeStyle(node.id)"
          :aria-label="`Inspect ${kindLabels[node.kind]} ${node.name}`"
          :aria-pressed="node.id === selectedNodeId"
          @click="selectNode(node.id)"
          @keydown="moveFocus($event, index)"
        )
          svg.node-icon(viewBox="0 0 24 24" aria-hidden="true")
            template(v-if="node.kind === 'listener' || node.kind === 'forward_proxy_listener'")
              path(d="M5 21V9a7 7 0 0 1 14 0v12")
              path(d="M9 21V10h6v11M13 15h.01")
            template(v-else-if="node.kind === 'rtmp_listener'")
              circle(cx="12" cy="12" r="2")
              path(d="M8.5 8.5a5 5 0 0 0 0 7M15.5 8.5a5 5 0 0 1 0 7M5.5 5.5a9 9 0 0 0 0 13M18.5 5.5a9 9 0 0 1 0 13")
            template(v-else-if="node.kind === 'tls_profile'")
              path(d="M12 3 20 6v5c0 5-3.4 8.2-8 10-4.6-1.8-8-5-8-10V6l8-3Z")
              path(d="m9 12 2 2 4-5")
            template(v-else-if="node.kind === 'certificate'")
              path(d="M6 3h8l4 4v14H6Z")
              path(d="M14 3v5h4M9 12h6M9 16h4")
            template(v-else-if="node.kind === 'http_service' || node.kind === 'l4_service'")
              rect(x="3" y="7" width="18" height="10" rx="1")
              path(d="M7 11h10M7 14h6")
            template(v-else-if="node.kind === 'http_route'")
              path(d="m12 3 9 9-9 9-9-9Z")
              path(d="M8 12h8M13 9l3 3-3 3")
            template(v-else-if="node.kind === 'upstream_pool'")
              path(d="m7 3 10 0 5 9-5 9H7l-5-9Z")
              path(d="M8 12h8")
            template(v-else)
              circle(cx="12" cy="12" r="8")
              circle(cx="12" cy="12" r="2")
          span.node-copy
            span.node-kind {{ kindLabels[node.kind] }}
            strong {{ node.name }}
            span.node-identity(v-if="topologyIdentityLabel(node)") {{ topologyIdentityLabel(node) }}
            span.node-health(v-if="overlayFor(node.id)") {{ stateLabel(node.id) }}
            span.node-runtime-count(v-if="runtimeCountLabel(node.id)") {{ runtimeCountLabel(node.id) }}

      section.topology-relations(aria-labelledby="topology-relations-heading")
        h3#topology-relations-heading Topology relations
        p(v-if="topology.edges.length === 0") No configured relations.
        ul(v-else)
          li(v-for="edge in topology.edges" :key="edge.id")
            button.relation-link(type="button" :aria-label="relationLabel(edge)" @click="selectNode(edge.target)")
              strong {{ nodeName(edge.source) }}
              span {{ edgeLabels[edge.kind] }}
              strong {{ nodeName(edge.target) }}

    aside.inspector(v-if="selectedNode" aria-live="polite" aria-label="Topology node inspector")
      header.inspector-heading
        div
          span.node-kind {{ kindLabels[selectedNode.kind] }}
          h3 {{ selectedNode.name }}
        button.inspector-close(type="button" aria-label="Close inspector" @click="closeInspector") Close
      dl.inspector-identity
        div
          dt Stable ID
          dd
            code {{ selectedNode.id }}
        div
          dt Config path
          dd
            code {{ selectedNode.configPath }}
        div(v-if="selectedOverlay")
          dt Runtime state
          dd {{ stateLabels[selectedOverlay.state] }}
        div(v-if="selectedIdentity")
          dt {{ selectedNode.kind === 'endpoint' ? 'Endpoint identity' : 'Bind identity' }}
          dd {{ selectedIdentity }}
        div(v-if="selectedConfiguredLimit")
          dt {{ selectedConfiguredLimit.label }}
          dd {{ selectedConfiguredLimit.value }}
        div(v-if="selectedActiveConnections !== null")
          dt Active connections
          dd {{ formatCount(selectedActiveConnections) }}
        div(v-if="selectedActiveLeases !== null")
          dt Active leases
          dd {{ formatCount(selectedActiveLeases) }}
      section.inspector-block(aria-labelledby="config-attributes-heading")
        h4#config-attributes-heading Redacted config attributes
        pre {{ attributesJson }}
      section.inspector-block(v-if="selectedOverlay" aria-labelledby="runtime-overlay-heading")
        h4#runtime-overlay-heading Runtime overlay
        pre {{ runtimeJson }}
    .inspector-prompt(v-else)
      span 01
      p Select any symbol to inspect its exact redacted attributes and stable config path.
</template>

<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'

import { formatCount } from './formatters'
import type {
  TopologyEdgeKind,
  TopologyEdge,
  TopologyNode,
  TopologyNodeKind,
  TopologyRuntimeOverlay,
  TopologyRuntimeState,
  TopologySnapshot,
} from './api'

const props = defineProps<{ topology: TopologySnapshot }>()

const CANVAS_WIDTH = 1_120
const NODE_WIDTH = 168
const NODE_HEIGHT = 88
const ROW_GAP = 124
const TOP_PADDING = 34
const columnX = [32, 258, 484, 710, 936]
const kindStage: Record<TopologyNodeKind, number> = {
  listener: 0,
  forward_proxy_listener: 0,
  rtmp_listener: 0,
  tls_profile: 1,
  certificate: 2,
  http_service: 1,
  http_route: 2,
  l4_service: 1,
  upstream_pool: 3,
  endpoint: 4,
}
const kindLabels: Record<TopologyNodeKind, string> = {
  listener: 'Listener',
  forward_proxy_listener: 'Forward proxy listener',
  rtmp_listener: 'RTMP listener',
  tls_profile: 'TLS profile',
  certificate: 'Certificate',
  http_service: 'HTTP service',
  http_route: 'HTTP route',
  l4_service: 'L4 service',
  upstream_pool: 'Upstream pool',
  endpoint: 'Endpoint',
}
const stateLabels: Record<TopologyRuntimeState, string> = {
  active: 'Active',
  available: 'Available',
  degraded: 'Degraded',
  unavailable: 'Unavailable',
  unchecked: 'Not monitored',
  unknown: 'Pending checks',
  healthy: 'Healthy',
  unhealthy: 'Unhealthy',
}
const SENSITIVE_TOPOLOGY_KEYS = new Set([
  'rootDirectory',
  'root_directory',
  'tokenFilePath',
  'token_file_path',
])
const edgeLabels: Record<TopologyEdgeKind, string> = {
  dispatch_service: 'Dispatches to service',
  service_route: 'Contains route',
  route_pool: 'Routes to pool',
  service_pool: 'Uses pool',
  pool_endpoint: 'Contains endpoint',
  listener_tls: 'Uses TLS profile',
  tls_certificate: 'Uses certificate',
}
const selectedNodeId = ref<string | null>(null)
const overlayByNode = computed(
  () => new Map(props.topology.overlays.map((overlay) => [overlay.nodeId, overlay])),
)
const orderedNodes = computed(() =>
  props.topology.nodes
    .map((node, sourceIndex) => ({ node, sourceIndex }))
    .sort(
      (left, right) =>
        kindStage[left.node.kind] - kindStage[right.node.kind] ||
        left.sourceIndex - right.sourceIndex,
    )
    .map(({ node }) => node),
)
const positions = computed(() => {
  const rows = [0, 0, 0, 0, 0]
  return new Map(
    orderedNodes.value.map((node) => {
      const stage = kindStage[node.kind]
      const row = rows[stage]!
      rows[stage] = row + 1
      return [node.id, { x: columnX[stage]!, y: TOP_PADDING + row * ROW_GAP }]
    }),
  )
})
const canvasHeight = computed(() => {
  const counts = [0, 0, 0, 0, 0]
  for (const node of orderedNodes.value) counts[kindStage[node.kind]]! += 1
  return Math.max(250, TOP_PADDING * 2 + Math.max(...counts) * ROW_GAP)
})
const stageStyle = computed(() => ({
  width: `${CANVAS_WIDTH}px`,
  height: `${canvasHeight.value}px`,
}))
const connectorPaths = computed(() =>
  props.topology.edges.flatMap((edge) => {
    const source = positions.value.get(edge.source)
    const target = positions.value.get(edge.target)
    if (!source || !target) return []
    const startX = source.x + NODE_WIDTH
    const startY = source.y + NODE_HEIGHT / 2
    const endX = target.x
    const endY = target.y + NODE_HEIGHT / 2
    const bend = Math.max(28, (endX - startX) / 2)
    return [
      {
        id: edge.id,
        kind: edge.kind,
        label: edgeLabels[edge.kind],
        path: `M ${startX} ${startY} C ${startX + bend} ${startY}, ${endX - bend} ${endY}, ${endX} ${endY}`,
      },
    ]
  }),
)
const selectedNode = computed(
  () => props.topology.nodes.find((node) => node.id === selectedNodeId.value) ?? null,
)
const selectedOverlay = computed(() =>
  selectedNode.value ? overlayFor(selectedNode.value.id) : undefined,
)
const selectedIdentity = computed(() =>
  selectedNode.value ? topologyIdentityLabel(selectedNode.value) : '',
)
const selectedConfiguredLimit = computed(() => {
  const node = selectedNode.value
  if (!node) return null
  if (node.kind === 'listener' || node.kind === 'forward_proxy_listener' || node.kind === 'rtmp_listener') {
    return 'maxConnections' in node.attributes
      ? { label: 'Connection limit', value: formatLimit(node.attributes.maxConnections) }
      : null
  }
  if (node.kind === 'http_service') {
    return 'maxRequestBodyBytes' in node.attributes
      ? { label: 'Request body limit', value: formatByteLimit(node.attributes.maxRequestBodyBytes) }
      : null
  }
  return null
})
const selectedActiveConnections = computed(() => numericMetric(selectedOverlay.value, 'activeConnections'))
const selectedActiveLeases = computed(() => decimalMetric(selectedOverlay.value, 'activeLeases'))
const attributesJson = computed(() => redactedJson(selectedNode.value?.attributes ?? {}))
const runtimeJson = computed(() => redactedJson(selectedOverlay.value?.metrics ?? {}))

function overlayFor(nodeId: string): TopologyRuntimeOverlay | undefined {
  return overlayByNode.value.get(nodeId)
}

function stateLabel(nodeId: string): string {
  const overlay = overlayFor(nodeId)
  return overlay ? stateLabels[overlay.state] : ''
}

function topologyIdentityLabel(node: TopologyNode): string {
  if (node.kind === 'listener' || node.kind === 'forward_proxy_listener' || node.kind === 'rtmp_listener') {
    const bind = node.attributes.bind
    if (!bind) return ''
    if (bind.type === 'unix') return `Unix / ${bind.path}`
    return `${bind.type === 'udp' ? 'UDP' : 'Socket'} / ${bind.address}`
  }
  if (node.kind !== 'endpoint') return ''
  switch (node.attributes.type) {
    case 'socket':
      return typeof node.attributes.address === 'string' ? `Socket / ${node.attributes.address}` : ''
    case 'dns':
      return typeof node.attributes.host === 'string' && typeof node.attributes.port === 'number'
        ? `DNS / ${node.attributes.host}:${node.attributes.port}`
        : ''
    case 'unix':
      return typeof node.attributes.path === 'string' ? `Unix / ${node.attributes.path}` : ''
    default:
      return ''
  }
}

function runtimeCountLabel(nodeId: string): string {
  const overlay = overlayFor(nodeId)
  const activeConnections = numericMetric(overlay, 'activeConnections')
  if (activeConnections !== null) return `${formatCount(activeConnections)} active connections`
  const activeLeases = decimalMetric(overlay, 'activeLeases')
  return activeLeases === null ? '' : `${formatCount(activeLeases)} active leases`
}

function numericMetric(overlay: TopologyRuntimeOverlay | undefined, key: string): number | null {
  const value = overlay?.metrics[key]
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null
}

function decimalMetric(overlay: TopologyRuntimeOverlay | undefined, key: string): string | null {
  const value = overlay?.metrics[key]
  return typeof value === 'string' && /^(0|[1-9][0-9]*)$/.test(value) ? value : null
}

function formatLimit(value: unknown): string {
  return value === null ? 'Unbounded' : typeof value === 'number' ? formatCount(value) : 'Unknown'
}

function formatByteLimit(value: unknown): string {
  return value === null ? 'Unbounded' : typeof value === 'number' ? `${formatCount(value)} bytes` : 'Unknown'
}

function nodeStyle(nodeId: string): Record<string, string> {
  const position = positions.value.get(nodeId)
  return position ? { left: `${position.x}px`, top: `${position.y}px` } : {}
}

function nodeName(nodeId: string): string {
  return props.topology.nodes.find((node) => node.id === nodeId)?.name ?? 'Unknown node'
}

function relationLabel(edge: TopologyEdge): string {
  return `${nodeName(edge.source)}: ${edgeLabels[edge.kind]} ${nodeName(edge.target)}`
}

function suppressSensitiveTopologyPaths(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(suppressSensitiveTopologyPaths)
  if (typeof value !== 'object' || value === null) return value
  return Object.fromEntries(
    Object.entries(value)
      .filter(([key]) => !SENSITIVE_TOPOLOGY_KEYS.has(key))
      .map(([key, entry]) => [key, suppressSensitiveTopologyPaths(entry)]),
  )
}

function redactedJson(value: unknown): string {
  return JSON.stringify(suppressSensitiveTopologyPaths(value), null, 2)
}

function selectNode(nodeId: string): void {
  selectedNodeId.value = nodeId
}

function closeInspector(): void {
  const nodeId = selectedNodeId.value
  if (!nodeId) return
  selectedNodeId.value = null
  void nextTick(() => {
    const node = Array.from(document.querySelectorAll<HTMLButtonElement>('[data-node-id]')).find(
      (candidate) => candidate.dataset.nodeId === nodeId,
    )
    node?.focus()
  })
}

function moveFocus(event: KeyboardEvent, index: number): void {
  let nextIndex = index
  if (['ArrowRight', 'ArrowDown'].includes(event.key)) nextIndex = index + 1
  else if (['ArrowLeft', 'ArrowUp'].includes(event.key)) nextIndex = index - 1
  else if (event.key === 'Home') nextIndex = 0
  else if (event.key === 'End') nextIndex = orderedNodes.value.length - 1
  else return

  event.preventDefault()
  const stage = (event.currentTarget as HTMLElement).closest('.topology-stage')
  const buttons = stage?.querySelectorAll<HTMLButtonElement>('.topology-node')
  if (!buttons?.length) return
  buttons[(nextIndex + buttons.length) % buttons.length]?.focus()
}
</script>

<style scoped>
.topology-section {
  padding: clamp(42px, 7vw, 74px) 0 12px;
}

.topology-heading,
.topology-state,
.inspector-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.eyebrow,
.node-kind,
.inspector-block h4,
dt {
  margin: 0;
  color: #929a88;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.66rem;
  font-weight: 650;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
p {
  margin-top: 0;
}

h2 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(2rem, 5vw, 3.7rem);
  font-weight: 400;
  letter-spacing: -0.045em;
}

.topology-deck {
  margin: 9px 0 0;
  color: #858d7d;
  font-size: 0.82rem;
}

.topology-state {
  justify-content: flex-start;
  color: #cbd0c2;
  font-size: 0.76rem;
}

.state-light {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #b6ff51;
  box-shadow: 0 0 14px rgb(182 255 81 / 68%);
}

.topology-workspace {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 300px;
  margin-top: 24px;
  border-block: 1px solid #34392f;
  background:
    linear-gradient(90deg, transparent 0 19%, rgb(182 255 81 / 2%) 19% 20%, transparent 20% 100%),
    rgb(12 15 11 / 54%);
}

.schematic-viewport {
  min-width: 0;
  overflow-x: auto;
  border-right: 1px solid #34392f;
}

.topology-relations {
  display: none;
}

.topology-stage {
  position: relative;
  min-height: 250px;
}

.connector-layer {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  overflow: visible;
}

.connector {
  fill: none;
  stroke: #526048;
  stroke-width: 1.25;
  vector-effect: non-scaling-stroke;
}

.edge-listener-tls,
.edge-tls-certificate {
  stroke: #786ba4;
}

#topology-arrow path {
  fill: #78846d;
}

.topology-node {
  position: absolute;
  display: grid;
  width: 168px;
  min-height: 88px;
  grid-template-columns: 42px minmax(0, 1fr);
  align-items: center;
  gap: 10px;
  padding: 7px 5px;
  border: 0;
  color: #dce2d4;
  background: transparent;
  cursor: pointer;
  text-align: left;
}

.topology-node::after {
  position: absolute;
  right: 0;
  bottom: 0;
  left: 52px;
  height: 1px;
  background: #30362d;
  content: "";
}

.topology-node:hover,
.topology-node.selected {
  color: #ffffff;
}

.topology-node:hover::after,
.topology-node.selected::after {
  height: 2px;
  background: #b6ff51;
}

.topology-node:focus-visible {
  outline: 2px solid #ffffff;
  outline-offset: 4px;
}

.node-icon {
  width: 38px;
  height: 38px;
  overflow: visible;
  color: #a9d875;
  fill: none;
  stroke: currentColor;
  stroke-linecap: round;
  stroke-linejoin: round;
  stroke-width: 1.35;
}

.node-tls_profile .node-icon,
.node-certificate .node-icon {
  color: #b8a6ff;
}

.node-http_route .node-icon {
  color: #e5bd67;
}

.node-rtmp_listener .node-icon {
  color: #efcf75;
}

.state-unavailable .node-icon,
.state-unhealthy .node-icon {
  color: #ff806b;
}

.state-degraded .node-icon,
.state-unknown .node-icon {
  color: #ffcf70;
}

.node-copy {
  display: grid;
  min-width: 0;
  gap: 3px;
}

.node-copy strong {
  overflow: hidden;
  font-size: 0.78rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.node-health,
.node-identity,
.node-runtime-count {
  overflow: hidden;
  color: #9ba493;
  font-size: 0.65rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.node-identity {
  color: #c5cdbd;
}

.inspector,
.inspector-prompt {
  min-width: 0;
  padding: 22px;
  background: rgb(19 22 17 / 94%);
}

.inspector-heading {
  align-items: flex-start;
  padding-bottom: 17px;
  border-bottom: 1px solid #34392f;
}

.inspector-heading h3 {
  margin: 5px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.45rem;
  font-weight: 400;
  overflow-wrap: anywhere;
}

.inspector-close {
  padding: 6px 0 6px 10px;
  border: 0;
  color: #aab1a2;
  background: transparent;
  cursor: pointer;
  font-size: 0.7rem;
}

.inspector-close:hover {
  color: #ffffff;
}

.inspector-close:focus-visible {
  outline: 2px solid #ffffff;
}

.inspector-identity {
  display: grid;
  gap: 13px;
  margin: 18px 0;
}

.inspector-identity div {
  display: grid;
  gap: 5px;
}

dd {
  margin: 0;
  color: #cbd1c3;
  font-size: 0.75rem;
}

code,
pre {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

code {
  overflow-wrap: anywhere;
}

.inspector-block {
  margin-top: 20px;
}

.inspector-block h4 {
  margin: 0 0 8px;
}

pre {
  max-height: 250px;
  margin: 0;
  padding: 12px;
  overflow: auto;
  color: #cbd1c3;
  background: #0c0f0b;
  font-size: 0.68rem;
  line-height: 1.55;
  white-space: pre-wrap;
  word-break: break-word;
}

.inspector-prompt {
  display: grid;
  align-content: center;
  gap: 12px;
  color: #858d7d;
}

.inspector-prompt span {
  color: #b6ff51;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 3rem;
}

.inspector-prompt p {
  margin: 0;
  font-size: 0.78rem;
  line-height: 1.55;
}

@media (max-width: 900px) {
  .topology-workspace {
    grid-template-columns: minmax(0, 1fr) 260px;
  }
}

@media (max-width: 700px) {
  .topology-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .topology-workspace {
    grid-template-columns: 1fr;
  }

  .schematic-viewport {
    border-right: 0;
    border-bottom: 1px solid #34392f;
  }

  .topology-stage {
    display: grid;
    width: auto !important;
    height: auto !important;
    padding: 8px 16px 14px;
  }

  .topology-stage::before {
    position: absolute;
    top: 22px;
    bottom: 28px;
    left: 39px;
    width: 1px;
    background: #526048;
    content: "";
  }

  .connector-layer {
    display: none;
  }

  .topology-relations {
    display: block;
    padding: 18px 16px;
    border-top: 1px solid #34392f;
  }

  .topology-relations h3 {
    margin-bottom: 12px;
    font-family: Georgia, "Times New Roman", serif;
    font-size: 1.25rem;
    font-weight: 400;
  }

  .topology-relations p {
    margin: 0;
    color: #858d7d;
    font-size: 0.78rem;
  }

  .topology-relations ul {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .relation-link {
    display: grid;
    width: 100%;
    min-height: 48px;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: 9px;
    padding: 10px;
    border: 1px solid #34392f;
    color: #dce2d4;
    background: #10130e;
    cursor: pointer;
    text-align: left;
  }

  .relation-link span {
    color: #929a88;
    font-size: 0.66rem;
    text-align: center;
  }

  .relation-link strong:last-child {
    text-align: right;
  }

  .relation-link:focus-visible {
    outline: 2px solid #ffffff;
    outline-offset: 2px;
  }

  .topology-node {
    position: relative !important;
    top: auto !important;
    left: auto !important;
    width: 100%;
    min-height: 72px;
    padding-left: 0;
  }

  .topology-node::before {
    position: absolute;
    top: 34px;
    left: 39px;
    width: 9px;
    height: 1px;
    background: #526048;
    content: "";
  }

  .node-icon {
    position: relative;
    z-index: 1;
    padding: 6px;
    background: #0d100c;
  }
}
</style>
