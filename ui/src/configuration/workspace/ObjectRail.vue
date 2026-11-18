<template lang="pug">
aside.object-rail
  label.mobile-nav-label(for="mobile-object-navigation") Current object
  Select#mobile-object-navigation.mobile-object-nav(v-model="selected" :options="mobileOptions" aria-label="Current configuration object")

  .mobile-add-controls(aria-label="Add configuration object")
    Button.mobile-add(
      v-for="group in groups"
      :key="group.collection"
      :aria-label="`Add ${group.singular}`"
      :disabled="compositional"
      :title="addTitle"
      @click="emit('add', group.collection)"
    ) + {{ group.singular }}

  nav.object-navigation(aria-label="Configuration objects" @keydown="moveConfigurationNavigationFocus")
    p.nav-section-label Core
    Button.object-link(
      v-for="item in coreItems"
      :key="item.key"
      :aria-current="selected === item.key ? 'page' : undefined"
      @click="selected = item.key"
    )
      span {{ item.label }}
      small {{ item.detail }}

    template(v-for="group in groups" :key="group.collection")
      .nav-group-heading(:data-field="group.collection")
        p.nav-section-label {{ group.label }}
        Button.nav-add(small :aria-label="`Add ${group.singular}`" :disabled="compositional" :title="addTitle" @click="emit('add', group.collection)") +
      p.nav-empty(v-if="group.items.length === 0") None
      Button.object-link(
        v-for="item in group.items"
        :key="item.key"
        :aria-current="selected === item.key ? 'page' : undefined"
        @click="selected = item.key"
      )
        span {{ item.label }}
        small {{ item.detail }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import Button from '#design/button/Button.vue'
import Select from '#design/field/Select.vue'
import type { CanonicalConfig } from '../../config'
import {
  moveConfigurationNavigationFocus,
  type CollectionName,
  type NavigationGroup,
  type NavigationItem,
} from '../useConfigurationNavigation'

const props = defineProps<{
  modelValue: string
  config: CanonicalConfig
  groups: NavigationGroup[]
  options: Array<NavigationItem & { group: string }>
  compositional: boolean
}>()
const emit = defineEmits<{
  'update:modelValue': [key: string]
  add: [collection: CollectionName]
}>()

const selected = computed({
  get: () => props.modelValue,
  set: (key: string) => emit('update:modelValue', key),
})
const addTitle = computed(() => (props.compositional ? 'Compositional roots are read-only.' : undefined))
const coreItems = computed<NavigationItem[]>(() => [
  { key: 'general', label: 'General', detail: 'Schema 1' },
  { key: 'management', label: 'Management', detail: props.config.management ? 'Configured' : 'Disabled' },
  {
    key: 'stats',
    label: 'Statistics',
    detail: props.config.stats
      ? `${props.config.stats.binds.length} observability binds / ${props.config.stats.pages.length} pages`
      : 'Disabled',
  },
])
const mobileOptions = computed(() => [
  ...coreItems.value.map(({ key, label }) => ({ value: key, label })),
  ...props.options.map(({ key, label, group }) => ({ value: key, label: `${group} / ${label}` })),
])
</script>
