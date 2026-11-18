<script setup vapor lang="ts">
import type { Tone } from './tone'
import './styles.css'

defineProps<{ tone: Tone }>()
</script>

<template lang="pug">
span.ox-dot(:class="`ox-tone-${tone}`" aria-hidden="true")
</template>
