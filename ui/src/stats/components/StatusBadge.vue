<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import { toneOf } from './status'

const props = defineProps<{ status: string; label?: string }>()
const tone = computed(() => toneOf(props.status))
const text = computed(() => props.label ?? props.status.toUpperCase())
</script>

<template lang="pug">
Badge(:tone="tone" :label="text")
</template>
