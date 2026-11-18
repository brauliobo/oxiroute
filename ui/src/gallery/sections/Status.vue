<script setup vapor lang="ts">
import StatusBadge from '#design/status/StatusBadge.vue'
import { FEED_STATUSES, GENERATION_STATUSES, LISTENER_STATUSES, POOL_STATUSES, SERVER_STATUSES } from '#design/status/status'
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-status")
  h2#demo-status Status words
  .ox-demo-row
    StatusBadge(v-for="status in SERVER_STATUSES" :key="status" :status="status")
  .ox-demo-row
    StatusBadge(v-for="status in POOL_STATUSES" :key="status" :status="status")
  .ox-demo-row
    StatusBadge(v-for="status in LISTENER_STATUSES" :key="status" :status="status")
    StatusBadge(v-for="status in GENERATION_STATUSES" :key="status" :status="status")
  .ox-demo-row
    StatusBadge(v-for="status in FEED_STATUSES" :key="status" :status="status")
</template>
