<script setup vapor lang="ts">
import './styles.css'

defineProps<{ label: string }>()
</script>

<template lang="pug">
.ox-scroll(role="region" tabindex="0" :aria-label="label")
  slot
</template>
