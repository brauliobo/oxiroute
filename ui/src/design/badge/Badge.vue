<script setup vapor lang="ts">
import { ICON_PATHS } from './tone'
import type { Tone } from './tone'
import './styles.css'

defineProps<{ tone: Tone; label: string }>()
</script>

<template lang="pug">
span.ox-badge(:class="`ox-tone-${tone}`")
  svg.ox-icon(viewBox="0 0 16 16" aria-hidden="true" focusable="false")
    path(:d="ICON_PATHS[tone]")
  span {{ label }}
</template>
