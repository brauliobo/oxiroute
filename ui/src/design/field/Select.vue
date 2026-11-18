<script setup vapor lang="ts">
import type { SelectOption } from './types'
import './styles.css'

defineProps<{ modelValue: string; options: readonly SelectOption[] }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const onChange = (event: Event) => emit('update:modelValue', (event.target as HTMLSelectElement).value)
</script>

<template lang="pug">
select.ox-input(@change="onChange")
  option(v-for="option in options" :key="option.value" :value="option.value" :selected="option.value === modelValue") {{ option.label }}
</template>
