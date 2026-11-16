<template lang="pug">
aside.object-rail
  label.mobile-nav-label(for="mobile-object-navigation") Current object
  select#mobile-object-navigation.mobile-object-nav(v-model="selected" aria-label="Current configuration object")
    option(v-for="item in coreItems" :key="item.key" :value="item.key") {{ item.label }}
    option(v-for="option in options" :key="option.key" :value="option.key") {{ option.group }} / {{ option.label }}

  .mobile-add-controls(aria-label="Add configuration object")
    button.mobile-add(
      v-for="group in groups"
      :key="group.collection"
      type="button"
      :aria-label="`Add ${group.singular}`"
      :disabled="compositional"
      :title="addTitle"
      @click="emit('add', group.collection)"
    ) + {{ group.singular }}

  nav.object-navigation(aria-label="Configuration objects" @keydown="moveConfigurationNavigationFocus")
    p.nav-section-label Core
    button.object-link(
      v-for="item in coreItems"
      :key="item.key"
      type="button"
      :aria-current="selected === item.key ? 'page' : undefined"
      @click="selected = item.key"
    )
      span {{ item.label }}
      small {{ item.detail }}

    template(v-for="group in groups" :key="group.collection")
      .nav-group-heading(:data-field="group.collection")
        p.nav-section-label {{ group.label }}
        button.nav-add(type="button" :aria-label="`Add ${group.singular}`" :disabled="compositional" :title="addTitle" @click="emit('add', group.collection)") +
      p.nav-empty(v-if="group.items.length === 0") None
      button.object-link(
        v-for="item in group.items"
        :key="item.key"
        type="button"
        :aria-current="selected === item.key ? 'page' : undefined"
        @click="selected = item.key"
      )
        span {{ item.label }}
        small {{ item.detail }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

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
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./fields.css"></style>
<style scoped>
.object-rail {
  min-width: 0;
  padding: 18px 0;
  border-right: 1px solid #34392f;
  background: #10130e;
}

.object-navigation {
  display: grid;
}

.nav-section-label {
  padding: 13px 18px 7px;
}

.nav-group-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 9px;
  border-top: 1px solid #292e26;
}

.nav-add {
  width: 32px;
  height: 32px;
  margin: 8px 11px 0 0;
  border: 1px solid #536345;
  color: #b6ff51;
  background: transparent;
  cursor: pointer;
}

.nav-empty {
  margin: 2px 18px 5px;
  color: #60675b;
  font-size: 0.7rem;
}

.object-link {
  position: relative;
  display: grid;
  width: 100%;
  gap: 3px;
  padding: 10px 18px;
  border: 0;
  color: #bec5b6;
  background: transparent;
  cursor: pointer;
  text-align: left;
}

.object-link::before {
  position: absolute;
  inset: 0 auto 0 0;
  width: 2px;
  background: transparent;
  content: "";
}

.object-link:hover,
.object-link[aria-current="page"] {
  color: #fff;
  background: #1b2017;
}

.object-link[aria-current="page"]::before {
  background: #b6ff51;
}

.object-link small {
  color: #70786a;
  font-size: 0.65rem;
}

.mobile-nav-label,
.mobile-object-nav,
.mobile-add-controls {
  display: none;
}

.mobile-add {
  min-height: 44px;
  padding: 8px 10px;
  border: 1px solid #536345;
  color: #c7ef94;
  background: transparent;
  cursor: pointer;
  text-align: left;
}

.mobile-add:focus-visible {
  outline: 2px solid #ffffff;
  outline-offset: 2px;
}

@media (max-width: 700px) {
  .object-rail {
    padding: 16px;
    border-right: 0;
    border-bottom: 1px solid #34392f;
  }

  .object-navigation {
    display: none;
  }

  .mobile-nav-label {
    display: block;
    margin-bottom: 7px;
    color: #929a88;
    font-size: 0.7rem;
  }

  .mobile-object-nav {
    display: block;
    min-height: 44px;
  }

  .mobile-add-controls {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
    margin-top: 12px;
  }
}
</style>
