<script setup vapor lang="ts">
import { THEMES } from '../urlState'
import type { Theme } from '../urlState'

defineProps<{ modelValue: Theme }>()
const emit = defineEmits<{ 'update:modelValue': [theme: Theme] }>()

const LABELS: Readonly<Record<Theme, string>> = { auto: 'Automatic', light: 'Light', dark: 'Dark' }
const choose = (event: Event) => emit('update:modelValue', (event.target as HTMLSelectElement).value as Theme)
</script>

<template lang="pug">
.ox-field
  label(for="ox-theme") Theme
  select#ox-theme.ox-input(name="theme" @change="choose")
    option(v-for="theme in THEMES" :key="theme" :value="theme" :selected="modelValue === theme") {{ LABELS[theme] }}
</template>
