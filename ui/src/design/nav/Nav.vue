<script setup vapor lang="ts">
import type { NavItem } from './types'
import './styles.css'

defineProps<{ items: readonly NavItem[]; current: string; label: string }>()
</script>

<template lang="pug">
nav.ox-nav(:aria-label="label")
  a.ox-nav-link(
    v-for="item in items"
    :key="item.id"
    :href="item.href"
    :aria-current="item.id === current ? 'page' : undefined"
  ) {{ item.label }}
</template>
