<script setup vapor lang="ts">
import './styles.css'

defineProps<{ expanded: boolean }>()
</script>

<template lang="pug">
button.ox-toggle(type="button" :aria-expanded="expanded")
  span.ox-chevron(aria-hidden="true")
  span.ox-label
    slot
</template>
