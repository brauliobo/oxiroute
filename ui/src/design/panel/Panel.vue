<script setup vapor lang="ts">
import { useId } from 'vue'
import './styles.css'

defineProps<{ title: string }>()
const headingId = useId()
</script>

<template lang="pug">
section.ox-panel(:aria-labelledby="headingId")
  h2(:id="headingId") {{ title }}
  slot
</template>
