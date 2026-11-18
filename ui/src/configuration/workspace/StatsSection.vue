<template lang="pug">
SectionHeading(eyebrow="Optional object" title="Statistics" path="Config.stats")
ConfigField(path="stats" label="Enable observability binds and/or standalone statistics pages" check :model-value="config.stats != null" @change="toggleStats")
.field-grid(v-if="config.stats")
  ConfigField(path="stats.binds" label="Bind addresses")
    Input(:model-value="config.stats.binds.join(', ')" placeholder="127.0.0.1:8404, [::1]:8404" @input="setBinds")
    small Comma-separated IPv4/IPv6 sockets.
  ConfigField(path="stats.admin_token_file" label="Admin token file")
    Input(:model-value="config.stats.admin_token_file ?? ''" placeholder="/etc/oxiroute/stats-admin.token" @input="setAdminTokenFile")
    small Optional; without it, administration remains disabled.
ConfigGroup.route-list(v-if="config.stats" path="stats.pages")
  template(#legend)
    .route-heading
      legend HAProxy-compatible pages
      Button.add-row(
        :disabled="config.stats.binds.length + config.stats.pages.length >= MAX_BINDS_AND_PAGES"
        title="The server allows at most eight statistics binds and pages in total."
        @click="addPage"
      ) + Add statistics page
  p.empty-list(v-if="config.stats.pages.length === 0") No standalone statistics pages configured.
  ItemScope.route-card(v-for="(page, pageIndex) in config.stats.pages" :key="pageIndex" :index="pageIndex")
    header.route-card-heading
      strong Statistics page {{ pageIndex + 1 }}
      Button.danger-link(variant="danger" small :aria-label="`Remove statistics page ${pageIndex + 1}`" @click="removePage(pageIndex)") Remove
    .field-grid
      ConfigField(path="stats.pages[].bind" label="Bind address")
        Input(v-model="page.bind" placeholder="127.0.0.1:8404")
      ConfigField(path="stats.pages[].uri_prefix" label="URI prefix")
        Input(v-model="page.uri_prefix" placeholder="/stats")
      ConfigField(path="stats.pages[].refresh_ms" label="Refresh (milliseconds)")
        NumberInput(min="1" max="86400000" step="1" v-model="page.refresh_ms")
      ConfigField(path="stats.pages[].admin" label="Administration")
        Select(
          v-model="page.admin"
          :options="[{ value: 'disabled', label: 'Disabled' }, { value: 'localhost', label: 'Localhost only' }]"
        )
      ConfigField(path="stats.pages[].max_connections" label="Maximum connections")
        NumberInput(min="1" step="1" :model-value="page.max_connections ?? ''" placeholder="Unbounded" @input="page.max_connections = nullableIntegerInput($event)")
      ConfigField(path="stats.pages[].downstream_timeouts.client_timeout_ms" label="Client timeout (milliseconds)")
        NumberInput(min="1" max="86400000" step="1" :model-value="page.downstream_timeouts.client_timeout_ms ?? ''" placeholder="No timeout" @input="page.downstream_timeouts.client_timeout_ms = nullableIntegerInput($event)")
      ConfigField(path="stats.pages[].downstream_timeouts.request_timeout_ms" label="Request-header timeout (milliseconds)")
        NumberInput(min="1" max="86400000" step="1" :model-value="page.downstream_timeouts.request_timeout_ms ?? ''" placeholder="Inherit client timeout" @input="page.downstream_timeouts.request_timeout_ms = nullableIntegerInput($event)")
      ConfigField(path="stats.pages[].downstream_timeouts.keepalive_timeout_ms" label="Keep-alive timeout (milliseconds)")
        NumberInput(min="1" max="86400000" step="1" :model-value="page.downstream_timeouts.keepalive_timeout_ms ?? ''" placeholder="Runtime default" @input="page.downstream_timeouts.keepalive_timeout_ms = nullableIntegerInput($event)")
    small The page is public and adds no routes beyond its URI prefix. Localhost administration accepts only same-origin loopback requests.
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { CanonicalConfig } from '../../config'
import { nullableInput, nullableIntegerInput } from './inputValues'
import SectionHeading from './SectionHeading.vue'
import ConfigField from '../fields/ConfigField.vue'
import ConfigGroup from '../fields/ConfigGroup.vue'
import ItemScope from '../fields/ItemScope.vue'

const MAX_BINDS_AND_PAGES = 8

const props = defineProps<{ config: CanonicalConfig }>()

function toggleStats(event: Event): void {
  props.config.stats = (event.target as HTMLInputElement).checked
    ? { binds: ['127.0.0.1:8404'], admin_token_file: null, pages: [] }
    : null
}

function setBinds(event: Event): void {
  if (!props.config.stats) return
  props.config.stats.binds = (event.target as HTMLInputElement).value
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)
}

function setAdminTokenFile(event: Event): void {
  if (props.config.stats) props.config.stats.admin_token_file = nullableInput(event)
}

function addPage(): void {
  const stats = props.config.stats
  if (!stats || stats.binds.length + stats.pages.length >= MAX_BINDS_AND_PAGES) return
  stats.pages.push({
    bind: '127.0.0.1:8405',
    uri_prefix: '/stats',
    refresh_ms: 10_000,
    admin: 'disabled',
    max_connections: null,
    downstream_timeouts: {
      client_timeout_ms: null,
      request_timeout_ms: null,
      keepalive_timeout_ms: null,
    },
  })
}

function removePage(index: number): void {
  props.config.stats?.pages.splice(index, 1)
}
</script>
