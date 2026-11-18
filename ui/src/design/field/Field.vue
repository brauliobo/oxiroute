<script setup vapor lang="ts">
import './styles.css'

defineProps<{ label?: string; labelFor?: string; hideLabel?: boolean; stacked?: boolean }>()
</script>

<template lang="pug">
.ox-field(:data-stacked="stacked || undefined")
  label(v-if="label" :for="labelFor" :class="{ 'ox-sr-only': hideLabel }") {{ label }}
  slot
</template>
