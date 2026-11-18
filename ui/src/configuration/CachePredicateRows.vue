<template lang="pug">
ItemScope.route-card(v-for="(predicate, index) in modelValue" :key="index" :index="index")
  header.cfg-route-card-heading
    strong Predicate {{ index + 1 }}
    Button.danger-link(variant="danger" small :aria-label="`Remove cache predicate ${index + 1}`" @click="remove(index)") Remove
  .field-grid
    ConfigField(:path="typeField" label="Predicate type")
      Select(
        v-model="predicate.type"
        :options="[{ value: 'header_present', label: 'Header present' }, { value: 'cookie_present', label: 'Cookie present' }]"
      )
    ConfigField(:path="nameField" :label="`${predicate.type === 'header_present' ? 'Header' : 'Cookie'} name`")
      Input(v-model="predicate.name")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
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
