<template lang="pug">
ConfigGroup.list-field(:path="fieldPath" :legend="label")
  p.field-hint(v-if="hint") {{ hint }}
  .list-row(v-for="(value, index) in modelValue" :key="index")
    label.ox-sr-only(:for="`${id}-${index}`") {{ itemLabel }} {{ index + 1 }}
    NumberInput(:id="`${id}-${index}`" :min="min" :max="max" step="1" :model-value="value" @input="update(index, $event)")
    Button.remove-row(variant="danger" small :disabled="modelValue.length <= minItems" :aria-label="`Remove ${itemLabel} ${index + 1}`" @click="remove(index)") Remove
  p.empty-list(v-if="modelValue.length === 0") No values configured.
  Button.add-row(:disabled="modelValue.length >= maxItems" @click="add") + Add {{ itemLabel }}
</template>

<script setup lang="ts" vapor>
import NumberInput from '#design/field/NumberInput.vue'
import Button from '#design/button/Button.vue'
import { useId } from 'vue'

import { integerInRange } from '../valueGuards'
import ConfigGroup from './fields/ConfigGroup.vue'

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
