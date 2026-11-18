<script setup vapor lang="ts">
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import { MAX_SEARCH_LENGTH } from '../urlState'
import { SEARCH_ID } from '../shortcuts'

const props = defineProps<{ modelValue: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const onKeydown = (event: KeyboardEvent) => {
  if (event.key === 'Escape' && props.modelValue) emit('update:modelValue', '')
}
</script>

<template lang="pug">
Field.ox-search(label="Search" :label-for="SEARCH_ID" hide-label)
  Input(
    :id="SEARCH_ID"
    type="search"
    autocomplete="off"
    spellcheck="false"
    placeholder="Search pools, servers, listeners ( / )"
    :maxlength="MAX_SEARCH_LENGTH"
    :model-value="modelValue"
    @update:modelValue="(value) => emit('update:modelValue', value)"
    @keydown="onKeydown"
  )
</template>
