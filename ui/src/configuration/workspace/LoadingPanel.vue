<template lang="pug">
.loading-panel(role="status" aria-live="polite")
  span.loading-mark(aria-hidden="true")
  div
    strong {{ title }}
    p {{ detail }}
</template>

<script setup lang="ts" vapor>
defineProps<{ title: string; detail: string }>()
</script>

<style scoped src="./panels.css"></style>
<style scoped>
.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: config-spin 800ms linear infinite;
}

@keyframes config-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
