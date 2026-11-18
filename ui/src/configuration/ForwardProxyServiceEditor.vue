<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow Canonical forward proxy
    h3 {{ service.name || 'Unnamed forward proxy' }}
    p.route-summary Configuration and validation are available even when runtime preflight reports this service as unsupported.
  Button.danger-button(variant="danger" @click="emit('remove')") Remove forward proxy

.field-grid
  ConfigField(path="forward_proxy_services[].name" label="Stable name")
    Input(v-model="service.name" required)
  ConfigField(path="forward_proxy_services[].audit_mode" label="Audit mode")
    Select(
      v-model="service.audit_mode"
      :options="[{ value: 'off', label: 'Off' }, { value: 'metadata', label: 'Metadata only' }]"
    )
  ConfigField.compact-enable(path="forward_proxy_services[].allow_absolute_form" label="Allow absolute-form requests" check v-model="service.allow_absolute_form")
  ConfigField.compact-enable(path="forward_proxy_services[].tls_required" label="Require TLS on network listeners" check v-model="service.tls_required")

ConfigGroup.retry-triggers(path="forward_proxy_services[].enabled_versions" legend="Enabled HTTP versions")
  label.enable-row(v-for="version in FORWARD_HTTP_VERSIONS" :key="version")
    input(type="checkbox" :checked="service.enabled_versions.includes(version)" :disabled="service.enabled_versions.length === 1 && service.enabled_versions.includes(version)" @change="toggleVersion(version, $event)")
    span {{ version.toUpperCase() }}

ConfigGroup.object-block(path="forward_proxy_services[].connect" legend="CONNECT tunneling")
  ConfigField(path="forward_proxy_services[].connect.enabled" label="Enable CONNECT authority tunneling" check v-model="service.connect.enabled")
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

ConfigGroup.object-block(path="forward_proxy_services[].connect_udp" legend="CONNECT-UDP tunneling")
  ConfigField(path="forward_proxy_services[].connect_udp.enabled" label="Enable CONNECT-UDP tunneling" check v-model="service.connect_udp.enabled")
  NumberListField(
    v-model="service.connect_udp.allowed_ports"
    label="Allowed CONNECT-UDP ports"
    item-label="port"
    field-path="forward_proxy_services[].connect_udp.allowed_ports"
    :default-value="443"
    :max="65535"
    :max-items="64"
    :min-items="service.connect_udp.enabled ? 1 : 0"
    hint="At least one unique nonzero port is required while CONNECT-UDP is enabled."
  )

ConfigGroup.object-block(path="forward_proxy_services[].peer_policy" legend="Static HTTP/1 peers")
  .field-grid
    ConfigField(path="forward_proxy_services[].peer_policy.direct_fallback" label="Direct fallback")
      Select(
        v-model="service.peer_policy.direct_fallback"
        :options="[{ value: 'allowed', label: 'Allowed' }, { value: 'denied', label: 'Denied' }, { value: 'required', label: 'Required' }]"
      )
    ConfigField(path="forward_proxy_services[].peer_policy.max_retries" label="Peer retry budget")
      NumberInput(min="0" max="15" step="1" v-model="service.peer_policy.max_retries")
  ConfigGroup.route-list(path="forward_proxy_services[].peer_policy.peers")
    template(#legend)
      .cfg-route-heading
        legend Peer endpoints
        Button.add-row(:disabled="service.peer_policy.peers.length >= 16" @click="addPeer") + Add peer
    p.empty-list(v-if="service.peer_policy.peers.length === 0") No static peers configured.
    ItemScope.route-card(v-for="(peer, peerIndex) in service.peer_policy.peers" :key="peerIndex" :index="peerIndex")
      header.cfg-route-card-heading
        strong Peer {{ peerIndex + 1 }}
        Button.danger-link(variant="danger" small :aria-label="`Remove forward peer ${peerIndex + 1}`" @click="removePeer(peerIndex)") Remove
      .field-grid
        ConfigField(path="forward_proxy_services[].peer_policy.peers[].host" label="Peer host")
          Input(v-model="peer.host" placeholder="proxy.example.test")
        ConfigField(path="forward_proxy_services[].peer_policy.peers[].port" label="Peer port")
          NumberInput(min="1" max="65535" step="1" v-model="peer.port")

ConfigGroup.object-block(path="forward_proxy_services[].auth" legend="Client authentication")
  Checkbox.enable-row(:model-value="service.auth !== null" @change="toggleAuth") Require credentials loaded from a server file
  .field-grid(v-if="service.auth")
    ConfigField(path="forward_proxy_services[].auth.type" label="Authentication type")
      Select(
        :model-value="service.auth.type" @change="changeAuthType"
        :options="[{ value: 'bearer_token_file', label: 'Bearer token file' }, { value: 'basic_htpasswd_file', label: 'Basic htpasswd file' }, { value: 'mutual_tls', label: 'Mutual TLS (reserved)' }]"
      )
    ConfigField(v-if="service.auth.type === 'bearer_token_file'" path="forward_proxy_services[].auth.token_file_path" label="Token file path")
      Input(v-model="service.auth.token_file_path" autocomplete="off" placeholder="/run/secrets/forward-proxy")
      small Authenticated configuration only; this path is suppressed from topology views.
    template(v-else-if="service.auth.type === 'basic_htpasswd_file'")
      ConfigField(path="forward_proxy_services[].auth.htpasswd_file_path" label="htpasswd file path")
        Input(v-model="service.auth.htpasswd_file_path" autocomplete="off" placeholder="/etc/oxiroute/proxy.htpasswd")
      ConfigField(path="forward_proxy_services[].auth.realm" label="Realm")
        Input(v-model="service.auth.realm")
      ConfigField(path="forward_proxy_services[].auth.credential_ttl_ms" label="Validated credential TTL (ms)")
        NumberInput(min="1" max="86400000" step="1" :model-value="service.auth.credential_ttl_ms ?? ''" @input="updateCredentialTtl")
      ConfigField.compact-enable(path="forward_proxy_services[].auth.username_case_sensitive" label="Match usernames case-sensitively" check v-model="service.auth.username_case_sensitive")
    ConfigField(v-else path="forward_proxy_services[].auth.client_ca_file_path" label="Client CA file path")
      Input(v-model="service.auth.client_ca_file_path" autocomplete="off" placeholder="/etc/oxiroute/client-ca.pem")

ConfigGroup.object-block(path="forward_proxy_services[].access_policy" legend="Ordered access policy")
  Checkbox.enable-row(:model-value="service.access_policy !== null" @change="toggleAccessPolicy") Evaluate first-match access rules
  template(v-if="service.access_policy")
    ConfigField(path="forward_proxy_services[].access_policy.default_action" label="Default action")
      Select(
        v-model="service.access_policy.default_action"
        :options="[{ value: 'deny', label: 'Deny' }, { value: 'allow', label: 'Allow' }]"
      )
    ConfigGroup.stack-list(path="forward_proxy_services[].access_policy.rules")
      ItemScope.object-block(v-for="(rule, ruleIndex) in service.access_policy.rules" :key="ruleIndex" :index="ruleIndex")
        .field-grid
          ConfigField(path="forward_proxy_services[].access_policy.rules[].action" label="Rule action")
            Select(
              v-model="rule.action"
              :options="[{ value: 'deny', label: 'Deny' }, { value: 'allow', label: 'Allow' }]"
            )
          Button.danger-button(variant="danger" @click="removeAccessRule(ruleIndex)") Remove rule
        ConfigGroup.stack-list(path="forward_proxy_services[].access_policy.rules[].conditions")
          ItemScope.object-block(v-for="(condition, conditionIndex) in rule.conditions" :key="conditionIndex" :index="conditionIndex")
            .field-grid
              ConfigField(path="forward_proxy_services[].access_policy.rules[].conditions[].type" label="Matcher")
                Select(
                  :model-value="condition.type" @change="changeConditionType(ruleIndex, conditionIndex, $event)"
                  :options="[{ value: 'all', label: 'All requests' }, { value: 'methods', label: 'Methods' }, { value: 'source_cidrs', label: 'Source CIDRs' }, { value: 'destination_ports', label: 'Destination ports' }, { value: 'authenticated', label: 'Authenticated' }, { value: 'destination_local', label: 'Destination local' }, { value: 'destination_link_local', label: 'Destination link-local' }, { value: 'manager', label: 'Manager' }]"
                )
              ConfigField.compact-enable(path="forward_proxy_services[].access_policy.rules[].conditions[].negated" label="Negate matcher" check v-model="condition.negated")
              Button.danger-button(variant="danger" @click="removeCondition(ruleIndex, conditionIndex)") Remove condition
            StringListField(v-if="condition.type === 'methods'" v-model="condition.methods" label="Methods" item-label="method" field-path="forward_proxy_services[].access_policy.rules[].conditions[].methods" :max-items="256")
            StringListField(v-if="condition.type === 'source_cidrs'" v-model="condition.cidrs" label="Source CIDRs" item-label="CIDR" field-path="forward_proxy_services[].access_policy.rules[].conditions[].cidrs" :max-items="256")
            ConfigGroup.stack-list(v-if="condition.type === 'destination_ports'" path="forward_proxy_services[].access_policy.rules[].conditions[].ranges")
              ItemScope.field-grid(v-for="(range, rangeIndex) in condition.ranges" :key="rangeIndex" :index="rangeIndex")
                ConfigField(path="forward_proxy_services[].access_policy.rules[].conditions[].ranges[].start" label="Start port")
                  NumberInput(min="1" max="65535" step="1" v-model="range.start")
                ConfigField(path="forward_proxy_services[].access_policy.rules[].conditions[].ranges[].end" label="End port")
                  NumberInput(:min="range.start" max="65535" step="1" v-model="range.end")
                Button.danger-button(variant="danger" :disabled="condition.ranges.length === 1" @click="condition.ranges.splice(rangeIndex, 1)") Remove range
              Button.secondary-button(@click="condition.ranges.push({ start: 443, end: 443 })") Add range
          Button.secondary-button(@click="addCondition(ruleIndex)") Add condition
      Button.secondary-button(@click="addAccessRule") Add rule

ConfigGroup.object-block(path="forward_proxy_services[].header_policy" legend="Forwarded metadata")
  .field-grid
    ConfigField(path="forward_proxy_services[].header_policy.forwarded_for" label="Forwarded and X-Forwarded-For")
      Select(
        v-model="service.header_policy.forwarded_for"
        :options="[{ value: 'delete', label: 'Delete' }, { value: 'preserve', label: 'Preserve' }]"
      )
    ConfigField(path="forward_proxy_services[].header_policy.via" label="Via")
      Select(
        v-model="service.header_policy.via"
        :options="[{ value: 'delete', label: 'Delete' }, { value: 'preserve', label: 'Preserve' }]"
      )

HttpCachePolicyEditor(
  :model-value="service.cache"
  field-path="forward_proxy_services[].cache"
  :store-names="cacheStoreNames"
  @update:model-value="service.cache = $event"
)

ConfigGroup.object-block(path="forward_proxy_services[].destination_policy" legend="Destination policy")
  ConfigField(path="forward_proxy_services[].destination_policy.deny_private" label="Deny private and special-purpose destinations" check v-model="service.destination_policy.deny_private")
  .field-grid
    StringListField(v-model="service.destination_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="forward_proxy_services[].destination_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.destination_policy.deny_domains" label="Denied domains" item-label="domain" field-path="forward_proxy_services[].destination_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.destination_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="forward_proxy_services[].destination_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.destination_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="forward_proxy_services[].destination_policy.deny_cidrs" :max-items="256")
  ConfigGroup.time-range-list(path="forward_proxy_services[].destination_policy.allow_times")
    template(#legend)
      .cfg-route-heading
        legend Allow windows
        Button.secondary-button(@click="addTimeRange('allow_times')") Add window
    ItemScope.object-block(v-for="(range, rangeIndex) in service.destination_policy.allow_times" :key="`allow-${rangeIndex}`" :index="rangeIndex")
      .field-grid
        ConfigField(path="forward_proxy_services[].destination_policy.allow_times[].days" label="Days")
          select.ox-input(multiple v-model="range.days")
            option(v-for="day in FORWARD_WEEKDAYS" :key="day" :value="day") {{ day }}
        ConfigField(path="forward_proxy_services[].destination_policy.allow_times[].start" label="Start (UTC)")
          input.ox-input(type="time" v-model="range.start")
        ConfigField(path="forward_proxy_services[].destination_policy.allow_times[].end" label="End (UTC)")
          input.ox-input(type="time" v-model="range.end")
        Button.danger-button(variant="danger" @click="removeTimeRange('allow_times', rangeIndex)") Remove window
  ConfigGroup.time-range-list(path="forward_proxy_services[].destination_policy.deny_times")
    template(#legend)
      .cfg-route-heading
        legend Deny windows
        Button.secondary-button(@click="addTimeRange('deny_times')") Add window
    ItemScope.object-block(v-for="(range, rangeIndex) in service.destination_policy.deny_times" :key="`deny-${rangeIndex}`" :index="rangeIndex")
      .field-grid
        ConfigField(path="forward_proxy_services[].destination_policy.deny_times[].days" label="Days")
          select.ox-input(multiple v-model="range.days")
            option(v-for="day in FORWARD_WEEKDAYS" :key="day" :value="day") {{ day }}
        ConfigField(path="forward_proxy_services[].destination_policy.deny_times[].start" label="Start (UTC)")
          input.ox-input(type="time" v-model="range.start")
        ConfigField(path="forward_proxy_services[].destination_policy.deny_times[].end" label="End (UTC)")
          input.ox-input(type="time" v-model="range.end")
        Button.danger-button(variant="danger" @click="removeTimeRange('deny_times', rangeIndex)") Remove window

fieldset.object-block
  legend Finite service limits
  .field-grid
    ConfigField(path="forward_proxy_services[].connect_timeout_ms" label="Connect timeout (ms)")
      NumberInput(min="1" max="86400000" step="1" v-model="service.connect_timeout_ms")
    ConfigField(path="forward_proxy_services[].idle_timeout_ms" label="Idle timeout (ms)")
      NumberInput(min="1" max="86400000" step="1" v-model="service.idle_timeout_ms")
    ConfigField(path="forward_proxy_services[].lifetime_timeout_ms" label="Lifetime timeout (ms)")
      NumberInput(min="1" max="86400000" step="1" v-model="service.lifetime_timeout_ms")
    ConfigField(path="forward_proxy_services[].max_request_body_bytes" label="Maximum request body bytes")
      NumberInput(min="1" max="1073741824" step="1" v-model="service.max_request_body_bytes" required)
      small Forward proxy validation requires a finite non-null value.
    ConfigField(path="forward_proxy_services[].max_header_bytes" label="Maximum header bytes")
      NumberInput(min="8192" max="1048576" step="1" v-model="service.max_header_bytes")
    ConfigField(path="forward_proxy_services[].max_connections" label="Maximum connections")
      NumberInput(min="1" max="1000000" step="1" v-model="service.max_connections")

ConfigGroup.object-block(path="forward_proxy_services[].resolver" legend="Bounded DNS resolver")
  StringListField(v-model="service.resolver.nameservers" label="Nameservers" item-label="IP address" field-path="forward_proxy_services[].resolver.nameservers" :max-items="8")
  .field-grid
    ConfigField(path="forward_proxy_services[].resolver.max_cache_entries" label="Maximum cache entries")
      NumberInput(min="1" max="1000000" step="1" v-model="service.resolver.max_cache_entries")
    ConfigField(path="forward_proxy_services[].resolver.max_concurrent_queries" label="Maximum concurrent queries")
      NumberInput(min="1" max="65536" step="1" v-model="service.resolver.max_concurrent_queries")
    ConfigField(path="forward_proxy_services[].resolver.max_addresses_per_name" label="Maximum addresses per name")
      NumberInput(min="1" max="256" step="1" v-model="service.resolver.max_addresses_per_name")
    ConfigField(path="forward_proxy_services[].resolver.min_ttl_ms" label="Minimum TTL (ms)")
      NumberInput(min="1" :max="service.resolver.max_ttl_ms" step="1" v-model="service.resolver.min_ttl_ms")
    ConfigField(path="forward_proxy_services[].resolver.max_ttl_ms" label="Maximum TTL (ms)")
      NumberInput(:min="service.resolver.min_ttl_ms" max="86400000" step="1" v-model="service.resolver.max_ttl_ms")
    ConfigField(path="forward_proxy_services[].resolver.negative_ttl_ms" label="Negative TTL (ms)")
      NumberInput(min="0" max="86400000" step="1" v-model="service.resolver.negative_ttl_ms")
    ConfigField.compact-enable(path="forward_proxy_services[].resolver.revalidate_on_connect" label="Revalidate on connect" check v-model="service.resolver.revalidate_on_connect")
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import StringListField from '../StringListField.vue'
import type {
  ForwardAccessConditionConfig,
  ForwardHttpVersion,
  ForwardProxyServiceConfig,
} from '../config'
import {
  FORWARD_HTTP_VERSIONS,
  FORWARD_WEEKDAYS,
} from './canonicalDefaults'
import HttpCachePolicyEditor from './HttpCachePolicyEditor.vue'
import NumberListField from './NumberListField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{ service: ForwardProxyServiceConfig; cacheStoreNames: string[] }>()
const emit = defineEmits<{ remove: []; changed: [] }>()

function toggleVersion(version: ForwardHttpVersion, event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!props.service.enabled_versions.includes(version)) props.service.enabled_versions.push(version)
  } else if (props.service.enabled_versions.length > 1) {
    props.service.enabled_versions = props.service.enabled_versions.filter((entry) => entry !== version)
  }
}

function addPeer(): void {
  if (props.service.peer_policy.peers.length >= 16) return
  props.service.peer_policy.peers.push({ host: '', port: 3128 })
  emit('changed')
}

function removePeer(index: number): void {
  props.service.peer_policy.peers.splice(index, 1)
  emit('changed')
}

function toggleAuth(event: Event): void {
  props.service.auth = (event.target as HTMLInputElement).checked
    ? { type: 'bearer_token_file', token_file_path: '' }
    : null
}

function changeAuthType(event: Event): void {
  const type = (event.target as HTMLSelectElement).value
  props.service.auth = type === 'basic_htpasswd_file'
    ? { type: 'basic_htpasswd_file', htpasswd_file_path: '', realm: 'Proxy', credential_ttl_ms: null, username_case_sensitive: true }
    : type === 'mutual_tls'
      ? { type: 'mutual_tls', client_ca_file_path: '' }
      : { type: 'bearer_token_file', token_file_path: '' }
}

function updateCredentialTtl(event: Event): void {
  if (props.service.auth?.type !== 'basic_htpasswd_file') return
  const value = (event.target as HTMLInputElement).value
  props.service.auth.credential_ttl_ms = value === '' ? null : Number(value)
}

function toggleAccessPolicy(event: Event): void {
  props.service.access_policy = (event.target as HTMLInputElement).checked
    ? { rules: [], default_action: 'deny' }
    : null
}

function addAccessRule(): void {
  props.service.access_policy?.rules.push({
    action: 'deny',
    conditions: [{ negated: false, type: 'all' }],
  })
}

function removeAccessRule(index: number): void {
  props.service.access_policy?.rules.splice(index, 1)
}

function addCondition(ruleIndex: number): void {
  props.service.access_policy?.rules[ruleIndex]?.conditions.push({ negated: false, type: 'all' })
}

function removeCondition(ruleIndex: number, conditionIndex: number): void {
  props.service.access_policy?.rules[ruleIndex]?.conditions.splice(conditionIndex, 1)
}

function changeConditionType(ruleIndex: number, conditionIndex: number, event: Event): void {
  const conditions = props.service.access_policy?.rules[ruleIndex]?.conditions
  const current = conditions?.[conditionIndex]
  if (!conditions || !current) return
  const negated = current.negated
  const type = (event.target as HTMLSelectElement).value
  let replacement: ForwardAccessConditionConfig
  if (type === 'methods') replacement = { negated, type, methods: ['GET'] }
  else if (type === 'source_cidrs') replacement = { negated, type, cidrs: ['127.0.0.0/8'] }
  else if (type === 'destination_ports') replacement = { negated, type, ranges: [{ start: 443, end: 443 }] }
  else replacement = { negated, type: type as 'all' | 'authenticated' | 'destination_local' | 'destination_link_local' | 'manager' }
  conditions[conditionIndex] = replacement
}

function addTimeRange(kind: 'allow_times' | 'deny_times'): void {
  props.service.destination_policy[kind].push({
    days: ['monday', 'tuesday', 'wednesday', 'thursday', 'friday'],
    start: '09:00',
    end: '17:00',
  })
}

function removeTimeRange(kind: 'allow_times' | 'deny_times', index: number): void {
  props.service.destination_policy[kind].splice(index, 1)
}
</script>
