<script setup vapor lang="ts">
import type { Fact } from './types'
import './styles.css'

defineProps<{ items: readonly Fact[] }>()
</script>

<template lang="pug">
dl.ox-facts
  .ox-fact(v-for="fact in items" :key="fact.label")
    dt {{ fact.label }}
    dd(:class="{ 'ox-mono': fact.code }") {{ fact.value }}
</template>
