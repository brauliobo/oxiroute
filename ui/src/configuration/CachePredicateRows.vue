<template lang="pug">
article.route-card(v-for="(predicate, index) in modelValue" :key="index")
  header.route-card-heading
    strong Predicate {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove cache predicate ${index + 1}`" @click="remove(index)") Remove
  .field-grid
    label.field(:data-field="typeField")
      span Predicate type
      select(v-model="predicate.type")
        option(value="header_present") Header present
        option(value="cookie_present") Cookie present
    label.field(:data-field="nameField")
      span {{ predicate.type === 'header_present' ? 'Header' : 'Cookie' }} name
      input(type="text" v-model="predicate.name")
</template>

<script setup lang="ts">
import type { CachePredicateConfig } from '../config'

const props = defineProps<{
  modelValue: CachePredicateConfig[]
  typeField: string
  nameField: string
}>()
const emit = defineEmits<{ 'update:modelValue': [value: CachePredicateConfig[]] }>()

function remove(index: number): void {
  emit('update:modelValue', props.modelValue.filter((_, entryIndex) => entryIndex !== index))
}
</script>
