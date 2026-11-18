<script setup vapor lang="ts">
import { useId } from 'vue'
import type { TabItem } from './types'
import './styles.css'

const props = defineProps<{ tabs: readonly TabItem[]; modelValue: string; label: string }>()
const emit = defineEmits<{ 'update:modelValue': [id: string] }>()

const uid = useId()
const panelId = `${uid}-panel`
const tabId = (id: string) => `${uid}-tab-${id}`

function select(id: string): void {
  emit('update:modelValue', id)
  document.getElementById(tabId(id))?.focus()
}

function target(key: string, index: number): number | undefined {
  const last = props.tabs.length - 1
  return { ArrowRight: index === last ? 0 : index + 1, ArrowLeft: index === 0 ? last : index - 1, Home: 0, End: last }[key]
}

function onKeydown(event: KeyboardEvent): void {
  const index = props.tabs.findIndex((tab) => tab.id === props.modelValue)
  const next = props.tabs[target(event.key, index) ?? -1]
  if (!next) return
  event.preventDefault()
  select(next.id)
}
</script>

<template lang="pug">
.ox-tabs
  .ox-tablist(role="tablist" :aria-label="label" @keydown="onKeydown")
    button.ox-tab(
      v-for="tab in tabs"
      :key="tab.id"
      :id="tabId(tab.id)"
      type="button"
      role="tab"
      :aria-selected="tab.id === modelValue"
      :aria-controls="panelId"
      :tabindex="tab.id === modelValue ? 0 : -1"
      @click="select(tab.id)"
    ) {{ tab.label }}
  .ox-tabpanel(:id="panelId" role="tabpanel" tabindex="0" :aria-labelledby="tabId(modelValue)")
    slot
</template>
