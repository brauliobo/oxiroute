<template lang="pug">
ConfigGroup.list-field(:path="fieldPath" :legend="label")
  p.field-hint(v-if="hint") {{ hint }}
  .list-row(v-for="(value, index) in modelValue" :key="index")
    label.ox-sr-only(:for="`${id}-${index}`") {{ itemLabel }} {{ index + 1 }}
    Input(:id="`${id}-${index}`" :model-value="value" :list="suggestions.length ? `${id}-suggestions` : undefined" :disabled="disabled" @input="update(index, $event)")
    Button.remove-row(variant="danger" small :disabled="disabled" :aria-label="`Remove ${itemLabel} ${index + 1}`" @click="remove(index)") Remove
  p.empty-list(v-if="modelValue.length === 0") No values configured.
  datalist(v-if="suggestions.length" :id="`${id}-suggestions`")
    option(v-for="suggestion in suggestions" :key="suggestion" :value="suggestion")
  Button.add-row(
    :disabled="disabled || modelValue.length >= maxItems"
    :title="modelValue.length >= maxItems ? `At most ${maxItems} ${itemLabel} values are supported.` : undefined"
    @click="add"
  ) + Add {{ itemLabel }}
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import Button from '#design/button/Button.vue'
import { useId } from 'vue'
import ConfigGroup from './configuration/fields/ConfigGroup.vue'

const props = withDefaults(
  defineProps<{
    modelValue: string[]
    label: string
    itemLabel: string
    fieldPath: string
    hint?: string
    suggestions?: string[]
    maxItems?: number
    disabled?: boolean
  }>(),
  { hint: '', suggestions: () => [], maxItems: Number.MAX_SAFE_INTEGER, disabled: false },
)
const emit = defineEmits<{ 'update:modelValue': [value: string[]] }>()
const id = useId()

function add(): void {
  if (props.disabled || props.modelValue.length >= props.maxItems) return
  emit('update:modelValue', [...props.modelValue, ''])
}

function remove(index: number): void {
  if (props.disabled) return
  emit('update:modelValue', props.modelValue.filter((_, valueIndex) => valueIndex !== index))
}

function update(index: number, event: Event): void {
  if (props.disabled) return
  const values = [...props.modelValue]
  values[index] = (event.target as HTMLInputElement).value
  emit('update:modelValue', values)
}
</script>
