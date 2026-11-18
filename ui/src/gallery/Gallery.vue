<script setup vapor lang="ts">
import { ref } from 'vue'
import { DENSITIES, useDensity } from '#design/composables/useDensity'
import type { Density } from '#design/composables/useDensity'
import { THEMES, useTheme } from '#design/composables/useTheme'
import type { Theme } from '#design/composables/useTheme'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'
import Shell from '#design/layout/Shell.vue'
import Actions from './sections/Actions.vue'
import Data from './sections/Data.vue'
import Feedback from './sections/Feedback.vue'
import Forms from './sections/Forms.vue'
import Navigation from './sections/Navigation.vue'

const params = new URLSearchParams(location.search)
const theme = ref<Theme>(THEMES.find((item) => item === params.get('theme')) ?? 'auto')
const density = ref<Density>(DENSITIES.find((item) => item === params.get('density')) ?? 'compact')
const densityOptions = DENSITIES.map((item) => ({ value: item, label: item }))

const setDensity = (value: string) => { density.value = value as Density }

useTheme(theme)
useDensity(density)
</script>

<template lang="pug">
Shell(title="Design gallery" skip-label="Skip to the primitives" v-model:theme="theme")
  template(v-slot:header)
    Field.ox-demo-controls(label="Density" label-for="ox-density")
      Select#ox-density(:model-value="density" :options="densityOptions" @update:modelValue="setDensity")
  Actions
  Forms
  Data
  Feedback
  Navigation
</template>
