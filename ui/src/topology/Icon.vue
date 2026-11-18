<script setup vapor lang="ts">
import type { TopologyNodeKind } from '../api'
import './styles.css'

defineProps<{ kind: TopologyNodeKind }>()
</script>

<template lang="pug">
svg.ox-node-icon(viewBox="0 0 24 24" aria-hidden="true" focusable="false")
  template(v-if="kind === 'listener' || kind === 'forward_proxy_listener'")
    path(d="M5 21V9a7 7 0 0 1 14 0v12")
    path(d="M9 21V10h6v11M13 15h.01")
  template(v-else-if="kind === 'rtmp_listener'")
    circle(cx="12" cy="12" r="2")
    path(d="M8.5 8.5a5 5 0 0 0 0 7M15.5 8.5a5 5 0 0 1 0 7M5.5 5.5a9 9 0 0 0 0 13M18.5 5.5a9 9 0 0 1 0 13")
  template(v-else-if="kind === 'tls_profile'")
    path(d="M12 3 20 6v5c0 5-3.4 8.2-8 10-4.6-1.8-8-5-8-10V6l8-3Z")
    path(d="m9 12 2 2 4-5")
  template(v-else-if="kind === 'certificate'")
    path(d="M6 3h8l4 4v14H6Z")
    path(d="M14 3v5h4M9 12h6M9 16h4")
  template(v-else-if="kind === 'http_service' || kind === 'l4_service'")
    rect(x="3" y="7" width="18" height="10" rx="1")
    path(d="M7 11h10M7 14h6")
  template(v-else-if="kind === 'http_route'")
    path(d="m12 3 9 9-9 9-9-9Z")
    path(d="M8 12h8M13 9l3 3-3 3")
  template(v-else-if="kind === 'upstream_pool'")
    path(d="m7 3 10 0 5 9-5 9H7l-5-9Z")
    path(d="M8 12h8")
  template(v-else)
    circle(cx="12" cy="12" r="8")
    circle(cx="12" cy="12" r="2")
</template>
