<template lang="pug">
header.form-heading
  div
    p.eyebrow Canonical forward proxy
    h3 {{ service.name || 'Unnamed forward proxy' }}
    p.route-summary Configuration and validation are available even when runtime preflight reports this service as unsupported.
  button.danger-button(type="button" @click="$emit('remove')") Remove forward proxy

.field-grid
  label.field(data-field="forward_proxy_services[].name")
    span Stable name
    input(type="text" v-model="service.name" required)
  label.field(data-field="forward_proxy_services[].audit_mode")
    span Audit mode
    select(v-model="service.audit_mode")
      option(value="off") Off
      option(value="metadata") Metadata only
  label.enable-row.compact-enable(data-field="forward_proxy_services[].allow_absolute_form")
    input(type="checkbox" v-model="service.allow_absolute_form")
    span Allow absolute-form requests
  label.enable-row.compact-enable(data-field="forward_proxy_services[].tls_required")
    input(type="checkbox" v-model="service.tls_required")
    span Require TLS on network listeners

fieldset.retry-triggers(data-field="forward_proxy_services[].enabled_versions")
  legend Enabled HTTP versions
  label.enable-row(v-for="version in FORWARD_HTTP_VERSIONS" :key="version")
    input(type="checkbox" :checked="service.enabled_versions.includes(version)" :disabled="service.enabled_versions.length === 1 && service.enabled_versions.includes(version)" @change="toggleVersion(version, $event)")
    span {{ version.toUpperCase() }}

fieldset.object-block(data-field="forward_proxy_services[].connect")
  legend CONNECT tunneling
  label.enable-row(data-field="forward_proxy_services[].connect.enabled")
    input(type="checkbox" v-model="service.connect.enabled")
    span Enable CONNECT authority tunneling
  NumberListField(
    v-model="service.connect.allowed_ports"
    label="Allowed CONNECT ports"
    item-label="port"
    field-path="forward_proxy_services[].connect.allowed_ports"
    :default-value="443"
    :max="65535"
    :max-items="64"
    :min-items="service.connect.enabled ? 1 : 0"
    hint="At least one unique nonzero port is required while CONNECT is enabled."
  )

fieldset.object-block(data-field="forward_proxy_services[].auth")
  legend Client authentication
  label.enable-row
    input(type="checkbox" :checked="service.auth !== null" @change="toggleAuth")
    span Require a bearer token loaded from a server file
  .field-grid(v-if="service.auth")
    label.field(data-field="forward_proxy_services[].auth.type")
      span Authentication type
      select(v-model="service.auth.type" disabled)
        option(value="bearer_token_file") Bearer token file
    label.field(data-field="forward_proxy_services[].auth.token_file_path")
      span Token file path
      input(type="text" v-model="service.auth.token_file_path" autocomplete="off" placeholder="/run/secrets/forward-proxy")
      small Authenticated configuration only; this path is suppressed from topology views.

fieldset.object-block(data-field="forward_proxy_services[].destination_policy")
  legend Destination policy
  label.enable-row(data-field="forward_proxy_services[].destination_policy.deny_private")
    input(type="checkbox" v-model="service.destination_policy.deny_private")
    span Deny private and special-purpose destinations
  .field-grid
    StringListField(v-model="service.destination_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="forward_proxy_services[].destination_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.destination_policy.deny_domains" label="Denied domains" item-label="domain" field-path="forward_proxy_services[].destination_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.destination_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="forward_proxy_services[].destination_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.destination_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="forward_proxy_services[].destination_policy.deny_cidrs" :max-items="256")

fieldset.object-block
  legend Finite service limits
  .field-grid
    label.field(data-field="forward_proxy_services[].connect_timeout_ms")
      span Connect timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.connect_timeout_ms")
    label.field(data-field="forward_proxy_services[].idle_timeout_ms")
      span Idle timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.idle_timeout_ms")
    label.field(data-field="forward_proxy_services[].lifetime_timeout_ms")
      span Lifetime timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.lifetime_timeout_ms")
    label.field(data-field="forward_proxy_services[].max_request_body_bytes")
      span Maximum request body bytes
      input(type="number" min="1" max="1073741824" step="1" v-model.number="service.max_request_body_bytes" required)
      small Forward proxy validation requires a finite non-null value.
    label.field(data-field="forward_proxy_services[].max_header_bytes")
      span Maximum header bytes
      input(type="number" min="1" max="1048576" step="1" v-model.number="service.max_header_bytes")
    label.field(data-field="forward_proxy_services[].max_connections")
      span Maximum connections
      input(type="number" min="1" max="1000000" step="1" v-model.number="service.max_connections")

fieldset.object-block(data-field="forward_proxy_services[].resolver")
  legend Bounded DNS resolver
  .field-grid
    label.field(data-field="forward_proxy_services[].resolver.max_cache_entries")
      span Maximum cache entries
      input(type="number" min="1" max="1000000" step="1" v-model.number="service.resolver.max_cache_entries")
    label.field(data-field="forward_proxy_services[].resolver.max_concurrent_queries")
      span Maximum concurrent queries
      input(type="number" min="1" max="65536" step="1" v-model.number="service.resolver.max_concurrent_queries")
    label.field(data-field="forward_proxy_services[].resolver.max_addresses_per_name")
      span Maximum addresses per name
      input(type="number" min="1" max="256" step="1" v-model.number="service.resolver.max_addresses_per_name")
    label.field(data-field="forward_proxy_services[].resolver.min_ttl_ms")
      span Minimum TTL (ms)
      input(type="number" min="1" :max="service.resolver.max_ttl_ms" step="1" v-model.number="service.resolver.min_ttl_ms")
    label.field(data-field="forward_proxy_services[].resolver.max_ttl_ms")
      span Maximum TTL (ms)
      input(type="number" :min="service.resolver.min_ttl_ms" max="86400000" step="1" v-model.number="service.resolver.max_ttl_ms")
    label.field(data-field="forward_proxy_services[].resolver.negative_ttl_ms")
      span Negative TTL (ms)
      input(type="number" min="0" max="86400000" step="1" v-model.number="service.resolver.negative_ttl_ms")
    label.enable-row.compact-enable(data-field="forward_proxy_services[].resolver.revalidate_on_connect")
      input(type="checkbox" v-model="service.resolver.revalidate_on_connect")
      span Revalidate DNS before CONNECT
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type { ForwardHttpVersion, ForwardProxyServiceConfig } from '../config'
import { FORWARD_HTTP_VERSIONS } from './canonicalDefaults'
import NumberListField from './NumberListField.vue'

const props = defineProps<{ service: ForwardProxyServiceConfig }>()
defineEmits<{ remove: [] }>()

function toggleVersion(version: ForwardHttpVersion, event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!props.service.enabled_versions.includes(version)) props.service.enabled_versions.push(version)
  } else if (props.service.enabled_versions.length > 1) {
    props.service.enabled_versions = props.service.enabled_versions.filter((entry) => entry !== version)
  }
}

function toggleAuth(event: Event): void {
  props.service.auth = (event.target as HTMLInputElement).checked
    ? { type: 'bearer_token_file', token_file_path: '' }
    : null
}
</script>
