<script setup vapor lang="ts">
import { text } from './present'
import type { Detail } from './present'
import './widgets.css'

defineProps<{ items: Detail[] }>()
</script>

<template lang="pug">
dl.ox-dl
  div(v-for="item in items" :key="item.label")
    dt {{ item.label }}
    dd(:title="item.title") {{ text(item.value) }}
</template>
