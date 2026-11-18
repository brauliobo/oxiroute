<script setup vapor lang="ts">
defineProps<{ kind: 'status' | 'alert' }>()
</script>

<template lang="pug">
div.ox-notice(:role="kind" aria-atomic="true")
  slot
</template>
