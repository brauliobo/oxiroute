<script setup vapor lang="ts">
import { useTemplateRef, watchPostEffect } from 'vue'
import './styles.css'

const props = defineProps<{ modelValue: number | string | null | undefined }>()
const emit = defineEmits<{ 'update:modelValue': [value: number | string] }>()
const input = useTemplateRef<HTMLInputElement>('input')

function onInput(event: Event) {
  const text = (event.target as HTMLInputElement).value
  const number = Number.parseFloat(text)
  emit('update:modelValue', Number.isNaN(number) ? text : number)
}

watchPostEffect(() => {
  const element = input.value
  if (!element) return
  const text = props.modelValue === null || props.modelValue === undefined ? '' : String(props.modelValue)
  if (element.value === text || (text !== '' && Number.parseFloat(element.value) === Number(props.modelValue))) return
  element.value = text
})
</script>

<template lang="pug">
input.ox-input(ref="input" type="number" @input="onInput")
</template>
