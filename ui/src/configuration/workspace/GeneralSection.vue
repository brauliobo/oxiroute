<template lang="pug">
SectionHeading(eyebrow="Root object" title="General" path="Config")
.field-grid
  label.field(data-field="version")
    span Version
    input(type="number" min="1" step="1" v-model.number="config.version")
    small Current canonical schema version.
</template>

<script setup lang="ts" vapor>
import type { CanonicalConfig } from '../../config'
import SectionHeading from './SectionHeading.vue'

defineProps<{ config: CanonicalConfig }>()
</script>

<style scoped src="./controls.css"></style>
<style scoped src="./fields.css"></style>
