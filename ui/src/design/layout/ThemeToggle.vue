<script setup vapor lang="ts">
import { THEMES, THEME_LABELS } from '../composables/useTheme'
import type { Theme } from '../composables/useTheme'
import Field from '../field/Field.vue'
import Select from '../field/Select.vue'

defineProps<{ modelValue: Theme }>()
const emit = defineEmits<{ 'update:modelValue': [theme: Theme] }>()

const options = THEMES.map((theme) => ({ value: theme, label: THEME_LABELS[theme] }))
const choose = (value: string) => emit('update:modelValue', value as Theme)
</script>

<template lang="pug">
Field(label="Theme" label-for="ox-theme")
  Select#ox-theme(name="theme" :model-value="modelValue" :options="options" @update:modelValue="choose")
</template>
