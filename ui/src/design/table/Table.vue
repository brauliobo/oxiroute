<script setup vapor lang="ts">
import './styles.css'

defineProps<{ caption: string; columns?: readonly string[] }>()
</script>

<template lang="pug">
table.ox-table
  caption.ox-sr-only {{ caption }}
  thead
    tr(v-if="columns")
      th(v-for="label in columns" :key="label" scope="col") {{ label }}
    slot(name="head")
  slot
</template>
