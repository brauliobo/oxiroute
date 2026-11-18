<template lang="pug">
ItemScope.route-card(v-for="(predicate, index) in modelValue" :key="index" :index="index")
  header.route-card-heading
    strong Predicate {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove cache predicate ${index + 1}`" @click="remove(index)") Remove
  .field-grid
    ConfigField(:path="typeField" label="Predicate type")
      select(v-model="predicate.type")
        option(value="header_present") Header present
        option(value="cookie_present") Cookie present
    ConfigField(:path="nameField" :label="`${predicate.type === 'header_present' ? 'Header' : 'Cookie'} name`")
      input(type="text" v-model="predicate.name")
</template>

<script setup lang="ts" vapor>
import type { CachePredicateConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ItemScope from './fields/ItemScope.vue'

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
