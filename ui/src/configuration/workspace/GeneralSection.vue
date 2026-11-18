<template lang="pug">
SectionHeading(eyebrow="Root object" title="General" path="Config")
.field-grid
  ConfigField(path="version" label="Version")
    NumberInput(min="1" step="1" v-model="config.version")
    small Current canonical schema version.
</template>

<script setup lang="ts" vapor>
import NumberInput from '#design/field/NumberInput.vue'
import type { CanonicalConfig } from '../../config'
import SectionHeading from './SectionHeading.vue'
import ConfigField from '../fields/ConfigField.vue'

defineProps<{ config: CanonicalConfig }>()
</script>
