<script setup vapor lang="ts">
import { useId } from 'vue'
import './styles.css'

defineProps<{ title: string; description: string; busy?: boolean }>()
const headingId = useId()
</script>

<template lang="pug">
section.ox-workspace(:aria-labelledby="headingId" :aria-busy="busy")
  header.ox-workspace-head
    div
      h2(:id="headingId") {{ title }}
      p.ox-muted {{ description }}
    .ox-toolbar
      slot(name="actions")
  slot
</template>
