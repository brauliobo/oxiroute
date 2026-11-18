<script setup vapor lang="ts">
import './styles.css'

defineProps<{ tone: 'ok' | 'info' | 'warn' | 'bad' }>()
</script>

<template lang="pug">
p.ox-banner(:data-tone="tone")
  span
    slot
  slot(name="actions")
</template>
