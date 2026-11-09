<script setup vapor lang="ts">
import { computed } from 'vue'
import { ICON_PATHS, toneOf } from './status'
import './widgets.css'

// Status as a shape and a word; the colour only repeats them.
const props = defineProps<{ status: string; label?: string }>()
const tone = computed(() => toneOf(props.status))
const icon = computed(() => ICON_PATHS[tone.value])
const text = computed(() => props.label ?? props.status.toUpperCase())
</script>

<template lang="pug">
span.ox-badge(:class="`ox-tone-${tone}`")
  svg.ox-icon(viewBox="0 0 16 16" aria-hidden="true" focusable="false")
    path(:d="icon")
  span {{ text }}
</template>
