<template lang="pug">
SectionHeading(eyebrow="Optional object" title="Statistics" path="Config.stats")
label.enable-row(data-field="stats")
  input(type="checkbox" :checked="config.stats != null" @change="toggleStats")
  span Enable observability binds and/or standalone statistics pages
.field-grid(v-if="config.stats")
  label.field(data-field="stats.binds")
    span Bind addresses
    input(type="text" :value="config.stats.binds.join(', ')" placeholder="127.0.0.1:8404, [::1]:8404" @input="setBinds")
    small Comma-separated IPv4/IPv6 sockets.
  label.field(data-field="stats.admin_token_file")
    span Admin token file
    input(type="text" :value="config.stats.admin_token_file ?? ''" placeholder="/etc/oxiroute/stats-admin.token" @input="setAdminTokenFile")
    small Optional; without it, administration remains disabled.
fieldset.route-list(v-if="config.stats" data-field="stats.pages")
  .route-heading
    legend HAProxy-compatible pages
    button.add-row(
      type="button"
      :disabled="config.stats.binds.length + config.stats.pages.length >= MAX_BINDS_AND_PAGES"
      title="The server allows at most eight statistics binds and pages in total."
      @click="addPage"
    ) + Add statistics page
  p.empty-list(v-if="config.stats.pages.length === 0") No standalone statistics pages configured.
  article.route-card(v-for="(page, pageIndex) in config.stats.pages" :key="pageIndex")
    header.route-card-heading
      strong Statistics page {{ pageIndex + 1 }}
      button.danger-link(type="button" :aria-label="`Remove statistics page ${pageIndex + 1}`" @click="removePage(pageIndex)") Remove
    .field-grid
      label.field(data-field="stats.pages[].bind")
        span Bind address
        input(type="text" v-model="page.bind" placeholder="127.0.0.1:8404")
      label.field(data-field="stats.pages[].uri_prefix")
        span URI prefix
        input(type="text" v-model="page.uri_prefix" placeholder="/stats")
      label.field(data-field="stats.pages[].refresh_ms")
        span Refresh (milliseconds)
        input(type="number" min="1" max="86400000" step="1" v-model.number="page.refresh_ms")
      label.field(data-field="stats.pages[].admin")
        span Administration
        select(v-model="page.admin")
          option(value="disabled") Disabled
          option(value="localhost") Localhost only
      label.field(data-field="stats.pages[].max_connections")
        span Maximum connections
        input(type="number" min="1" step="1" :value="page.max_connections ?? ''" placeholder="Unbounded" @input="page.max_connections = nullableIntegerInput($event)")
      label.field(data-field="stats.pages[].downstream_timeouts.client_timeout_ms")
        span Client timeout (milliseconds)
        input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.client_timeout_ms ?? ''" placeholder="No timeout" @input="page.downstream_timeouts.client_timeout_ms = nullableIntegerInput($event)")
      label.field(data-field="stats.pages[].downstream_timeouts.request_timeout_ms")
        span Request-header timeout (milliseconds)
        input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.request_timeout_ms ?? ''" placeholder="Inherit client timeout" @input="page.downstream_timeouts.request_timeout_ms = nullableIntegerInput($event)")
      label.field(data-field="stats.pages[].downstream_timeouts.keepalive_timeout_ms")
        span Keep-alive timeout (milliseconds)
        input(type="number" min="1" max="86400000" step="1" :value="page.downstream_timeouts.keepalive_timeout_ms ?? ''" placeholder="Runtime default" @input="page.downstream_timeouts.keepalive_timeout_ms = nullableIntegerInput($event)")
    small The page is public and adds no routes beyond its URI prefix. Localhost administration accepts only same-origin loopback requests.
</template>

<script setup lang="ts" vapor>
import type { CanonicalConfig } from '../../config'
import { nullableInput, nullableIntegerInput } from './inputValues'
import SectionHeading from './SectionHeading.vue'

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

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./fields.css"></style>
