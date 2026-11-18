<script setup vapor lang="ts">
import { computed, ref, useId } from 'vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import './page.css'

const props = defineProps<{
  label: string
  subject: string
  applyLabel: string
  resetLabel: string
  placeholder: string
  min: number
  max: number | null
  disabled: boolean
}>()
const emit = defineEmits<{ apply: [value: number]; reset: [] }>()

const inputId = useId()
const hintId = useId()
const draft = ref('')

const value = computed(() => (draft.value === '' ? null : Number(draft.value)))
const valid = computed(() => value.value !== null && Number.isInteger(value.value) && value.value >= props.min &&
  (props.max === null || value.value <= props.max))
const invalid = computed(() => draft.value !== '' && !valid.value)
const hint = computed(() => `Enter a whole number from ${props.min}${props.max === null ? ' up' : ` to ${props.max}`}.`)

function submit(): void {
  if (valid.value) emit('apply', value.value as number)
}

function reset(): void {
  draft.value = ''
  emit('reset')
}
</script>

<template lang="pug">
form.ox-ops-form(@submit.prevent="submit")
  Field(stacked :label="label" :label-for="inputId")
    Input(
      :id="inputId"
      v-model="draft"
      type="number"
      :min="min"
      :max="max ?? undefined"
      step="1"
      :placeholder="placeholder"
      :aria-label="`${label} of ${subject}`"
      :aria-invalid="invalid"
      :aria-describedby="invalid ? hintId : undefined"
    )
  Button(small type="submit" :disabled="disabled || !valid" :aria-label="`${applyLabel} for ${subject}`") {{ applyLabel }}
  Button(small :disabled="disabled" :aria-label="`${resetLabel} for ${subject}`" @click="reset") {{ resetLabel }}
  p.ox-ops-hint.ox-bad-text(v-if="invalid" :id="hintId") {{ hint }}
</template>
