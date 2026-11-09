<script setup vapor lang="ts">
import { computed } from 'vue'

const PRESETS_MS = [1000, 2000, 5000, 10_000, 30_000, 60_000]

const props = defineProps<{ modelValue: number | undefined; hintMs: number }>()
const emit = defineEmits<{ 'update:modelValue': [intervalMs: number | undefined] }>()

const seconds = (ms: number) => `${ms / 1000} s`
// An interval from a shared link that is not a preset stays selectable.
const options = computed(() => [...new Set([...PRESETS_MS, ...(props.modelValue === undefined ? [] : [props.modelValue])])].sort((a, b) => a - b))
const choose = (event: Event) => {
  const value = (event.target as HTMLSelectElement).value
  emit('update:modelValue', value ? Number(value) : undefined)
}
</script>

<template lang="pug">
.ox-field
  label(for="ox-interval") Refresh every
  select#ox-interval.ox-input(name="interval" @change="choose")
    option(value="" :selected="modelValue === undefined") Server hint ({{ seconds(hintMs) }})
    option(v-for="ms in options" :key="ms" :value="String(ms)" :selected="modelValue === ms") {{ seconds(ms) }}
</template>
