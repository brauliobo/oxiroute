<script setup vapor lang="ts">
import { MAX_SEARCH_LENGTH } from '../urlState'
import { SEARCH_ID } from '../shortcuts'

const props = defineProps<{ modelValue: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const onInput = (event: Event) => emit('update:modelValue', (event.target as HTMLInputElement).value)
const onKeydown = (event: KeyboardEvent) => {
  if (event.key === 'Escape' && props.modelValue) emit('update:modelValue', '')
}
</script>

<template lang="pug">
.ox-field.ox-search
  label.ox-sr-only(:for="SEARCH_ID") Search
  input.ox-input(
    :id="SEARCH_ID"
    type="search"
    autocomplete="off"
    spellcheck="false"
    placeholder="Search pools, servers, listeners ( / )"
    :maxlength="MAX_SEARCH_LENGTH"
    :value="modelValue"
    @input="onInput"
    @keydown="onKeydown"
  )
</template>
