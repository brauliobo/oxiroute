<template lang="pug">
fieldset.list-field(:data-field="fieldPath")
  legend {{ label }}
  p.field-hint(v-if="hint") {{ hint }}
  .list-row(v-for="(value, index) in modelValue" :key="index")
    label.sr-only(:for="`${id}-${index}`") {{ itemLabel }} {{ index + 1 }}
    input(
      :id="`${id}-${index}`"
      type="text"
      :value="value"
      :list="suggestions.length ? `${id}-suggestions` : undefined"
      :disabled="disabled"
      @input="update(index, $event)"
    )
    button.remove-row(type="button" :disabled="disabled" :aria-label="`Remove ${itemLabel} ${index + 1}`" @click="remove(index)") Remove
  p.empty-list(v-if="modelValue.length === 0") No values configured.
  datalist(v-if="suggestions.length" :id="`${id}-suggestions`")
    option(v-for="suggestion in suggestions" :key="suggestion" :value="suggestion")
  button.add-row(
    type="button"
    :disabled="disabled || modelValue.length >= maxItems"
    :title="modelValue.length >= maxItems ? `At most ${maxItems} ${itemLabel} values are supported.` : undefined"
    @click="add"
  ) + Add {{ itemLabel }}
</template>

<script setup lang="ts">
import { useId } from 'vue'

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

<style scoped>
.list-field {
  min-width: 0;
  margin: 0;
  padding: 0;
  border: 0;
}

legend {
  margin-bottom: 8px;
  color: #b8bfb0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.field-hint,
.empty-list {
  margin: -2px 0 9px;
  color: #7f8778;
  font-size: 0.72rem;
}

.list-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  margin-bottom: 8px;
}

input {
  min-width: 0;
  padding: 10px 11px;
  border: 1px solid #42493c;
  color: #eef2e7;
  background: #0d100c;
  font: 0.78rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

input:focus-visible,
button:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

button {
  min-height: 41px;
  border: 1px solid #4a5145;
  color: #b8c0b0;
  background: transparent;
  cursor: pointer;
}

@media (max-width: 700px) {
  button,
  input {
    min-height: 44px;
  }
}

.remove-row {
  padding-inline: 11px;
  color: #f39a8a;
}

.add-row {
  padding: 8px 11px;
  border-color: #647653;
  color: #c7ef94;
}

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}
</style>
