<template lang="pug">
fieldset.object-block.limit-control(:data-field="fieldPath")
  legend {{ legend }}
  .field-grid
    label.field
      span {{ modeLabel }}
      select(:value="modelValue === null ? 'unbounded' : 'bounded'" @change="changeMode")
        option(value="bounded") {{ boundedLabel }}
        option(value="unbounded") {{ unboundedLabel }}
    label.field(v-if="modelValue !== null")
      span {{ inputLabel }}
      input(type="number" :min="min" :max="max" step="1" :value="modelValue" @input="setLimit")
</template>

<script setup lang="ts">
import { integerInRange } from '../valueGuards'

const props = withDefaults(
  defineProps<{
    modelValue: number | null
    defaultValue: number
    fieldPath: string
    legend: string
    inputLabel: string
    min?: number
    max?: number
    modeLabel?: string
    boundedLabel?: string
    unboundedLabel?: string
  }>(),
  {
    min: 1,
    max: Number.MAX_SAFE_INTEGER,
    modeLabel: 'Limit mode',
    boundedLabel: 'Bounded',
    unboundedLabel: 'Unbounded',
  },
)

const emit = defineEmits<{ 'update:modelValue': [value: number | null] }>()

function changeMode(event: Event): void {
  const mode = (event.target as HTMLSelectElement).value
  emit('update:modelValue', mode === 'unbounded' ? null : props.modelValue ?? props.defaultValue)
}

function setLimit(event: Event): void {
  if (props.modelValue === null) return
  const input = event.target as HTMLInputElement
  const value = Number(input.value)
  if (integerInRange(value, props.min, props.max)) {
    emit('update:modelValue', value)
  } else {
    input.value = String(props.modelValue)
  }
}
</script>
