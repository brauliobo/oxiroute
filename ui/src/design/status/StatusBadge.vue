<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '../badge/Badge.vue'
import { statusLabel, toneOf } from './status'
import type { Status } from './status'

const props = defineProps<{ status: Status }>()
const label = computed(() => statusLabel(props.status))
const tone = computed(() => toneOf(props.status))
</script>

<template lang="pug">
Badge(:tone="tone" :label="label")
</template>
