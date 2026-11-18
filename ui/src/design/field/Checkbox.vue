<script setup vapor lang="ts">
import './styles.css'

defineProps<{ modelValue: boolean; disabled?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const onChange = (event: Event) => emit('update:modelValue', (event.target as HTMLInputElement).checked)
</script>

<template lang="pug">
label.ox-check
  input(type="checkbox" :checked="modelValue" :disabled="disabled" @change="onChange")
  span
    slot
</template>
