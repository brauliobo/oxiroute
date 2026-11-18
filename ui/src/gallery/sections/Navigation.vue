<script setup vapor lang="ts">
import { ref } from 'vue'
import Kbd from '#design/kbd/Kbd.vue'
import Nav from '#design/nav/Nav.vue'
import Tabs from '#design/tabs/Tabs.vue'

const ITEMS = [
  { id: 'overview', label: 'Overview', href: '#overview' },
  { id: 'operations', label: 'Operations', href: '#operations' },
  { id: 'audit', label: 'Audit', href: '#audit' },
]
const TABS = [{ id: 'general', label: 'General' }, { id: 'pools', label: 'Pools' }, { id: 'tls', label: 'TLS' }]
const tab = ref('general')
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-navigation")
  h2#demo-navigation Navigation, tabs and keys
  Nav(:items="ITEMS" current="operations" label="Gallery sections")
  Tabs(v-model="tab" :tabs="TABS" label="Configuration sections")
    p Panel of the {{ tab }} tab.
  .ox-demo-row
    p Press #[Kbd /] to search and #[Kbd Esc] to clear.
</template>
