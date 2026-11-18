<script setup vapor lang="ts">
import { computed } from 'vue'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'

const PRESETS_MS = [1000, 2000, 5000, 10_000, 30_000, 60_000]

const props = defineProps<{ modelValue: number | undefined; hintMs: number }>()
const emit = defineEmits<{ 'update:modelValue': [intervalMs: number | undefined] }>()

const seconds = (ms: number) => `${ms / 1000} s`
const choices = computed(() => [...new Set([...PRESETS_MS, ...(props.modelValue === undefined ? [] : [props.modelValue])])].sort((a, b) => a - b))
const options = computed(() => [
  { value: '', label: `Server hint (${seconds(props.hintMs)})` },
  ...choices.value.map((ms) => ({ value: String(ms), label: seconds(ms) })),
])
const choose = (value: string) => emit('update:modelValue', value ? Number(value) : undefined)
</script>

<template lang="pug">
Field(label="Refresh every" label-for="ox-interval")
  Select#ox-interval(name="interval" :model-value="modelValue === undefined ? '' : String(modelValue)" :options="options" @update:modelValue="choose")
</template>
