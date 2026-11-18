<script setup vapor lang="ts">
import './styles.css'

defineProps<{ pressed: boolean }>()
</script>

<template lang="pug">
button.ox-chip(type="button" :aria-pressed="pressed")
  slot
</template>
