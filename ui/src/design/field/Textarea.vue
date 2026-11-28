<script setup vapor lang="ts">
import './styles.css'

withDefaults(defineProps<{ modelValue: string; rows?: number; spellcheck?: boolean }>(), {
  rows: 18,
  spellcheck: false,
})
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const onInput = (event: Event) => emit('update:modelValue', (event.target as HTMLTextAreaElement).value)
</script>

<template lang="pug">
textarea.ox-input.ox-textarea(:rows="rows" :spellcheck="spellcheck" :value="modelValue" @input="onInput")
</template>
