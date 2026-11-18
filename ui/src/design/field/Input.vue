<script setup vapor lang="ts">
import './styles.css'

withDefaults(defineProps<{ modelValue: string; type?: 'text' | 'search' | 'password' | 'number' }>(), { type: 'text' })
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const onInput = (event: Event) => emit('update:modelValue', (event.target as HTMLInputElement).value)
</script>

<template lang="pug">
input.ox-input(:type="type" :value="modelValue" @input="onInput")
</template>
