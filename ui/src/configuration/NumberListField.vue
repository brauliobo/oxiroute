<template lang="pug">
fieldset.list-field(:data-field="fieldPath")
  legend {{ label }}
  p.field-hint(v-if="hint") {{ hint }}
  .list-row(v-for="(value, index) in modelValue" :key="index")
    label.sr-only(:for="`${id}-${index}`") {{ itemLabel }} {{ index + 1 }}
    input(
      :id="`${id}-${index}`"
      type="number"
      :min="min"
      :max="max"
      step="1"
      :value="value"
      @input="update(index, $event)"
    )
    button.remove-row(type="button" :disabled="modelValue.length <= minItems" :aria-label="`Remove ${itemLabel} ${index + 1}`" @click="remove(index)") Remove
  p.empty-list(v-if="modelValue.length === 0") No values configured.
  button.add-row(type="button" :disabled="modelValue.length >= maxItems" @click="add") + Add {{ itemLabel }}
</template>

<script setup lang="ts">
import { useId } from 'vue'

import { integerInRange } from '../valueGuards'

const props = withDefaults(defineProps<{
  modelValue: number[]
  label: string
  itemLabel: string
  fieldPath: string
  defaultValue: number
  min?: number
  max?: number
  maxItems?: number
  minItems?: number
  hint?: string
}>(), {
  min: 1,
  max: Number.MAX_SAFE_INTEGER,
  maxItems: Number.MAX_SAFE_INTEGER,
  minItems: 0,
  hint: '',
})
const emit = defineEmits<{ 'update:modelValue': [value: number[]] }>()
const id = useId()

function add(): void {
  if (props.modelValue.length < props.maxItems) emit('update:modelValue', [...props.modelValue, props.defaultValue])
}

function remove(index: number): void {
  if (props.modelValue.length <= props.minItems) return
  emit('update:modelValue', props.modelValue.filter((_, valueIndex) => valueIndex !== index))
}

function update(index: number, event: Event): void {
  const input = event.target as HTMLInputElement
  const value = Number(input.value)
  if (!integerInRange(value, props.min, props.max)) {
    input.value = String(props.modelValue[index])
    return
  }
  const values = [...props.modelValue]
  values[index] = value
  emit('update:modelValue', values)
}
</script>

<style scoped>
.list-field { min-width: 0; margin: 0; padding: 0; border: 0; }
legend { margin-bottom: 8px; color: #b8bfb0; font-size: 0.72rem; font-weight: 700; letter-spacing: 0.08em; text-transform: uppercase; }
.field-hint, .empty-list { margin: -2px 0 9px; color: #7f8778; font-size: 0.72rem; }
.list-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; margin-bottom: 8px; }
input { min-width: 0; padding: 10px 11px; border: 1px solid #42493c; color: #eef2e7; background: #0d100c; font: 0.78rem "IBM Plex Mono", monospace; }
button { min-height: 41px; padding-inline: 11px; border: 1px solid #4a5145; color: #f39a8a; background: transparent; cursor: pointer; }
.add-row { padding: 8px 11px; border-color: #647653; color: #c7ef94; }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; }
</style>
