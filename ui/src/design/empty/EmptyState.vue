<script setup vapor lang="ts">
import './styles.css'

defineProps<{ title?: string }>()
</script>

<template lang="pug">
.ox-empty
  strong(v-if="title") {{ title }}
  span
    slot
</template>
