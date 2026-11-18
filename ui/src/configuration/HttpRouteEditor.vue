<template lang="pug">
ItemScope.route-card.http-route-card(:index="index")
  header.cfg-route-card-heading
    div
      strong Route {{ index + 1 }}
      span.route-summary {{ routeSummary }}
    Button.danger-link(variant="danger" small :aria-label="`Remove route ${index + 1}`" @click="emit('remove')") Remove

  ConfigGroup.object-block(path="http_services[].routes[].host" legend="Host matcher")
    Checkbox.enable-row(:model-value="route.host !== null" @change="toggleHost") Match a specific host or authority
    .field-grid(v-if="route.host")
      ConfigField(path="http_services[].routes[].host.kind" label="Host match kind")
        Select(
          v-model="route.host.kind"
          :options="[{ value: 'normalized_host', label: 'Normalized host' }, { value: 'exact_authority', label: 'Exact authority' }, { value: 'ascii_case_insensitive_exact_authority', label: 'ASCII case-insensitive exact authority' }]"
        )
      ConfigField(path="http_services[].routes[].host.value" :label="`${route.host.kind.includes('authority') ? 'Authority' : 'Host'} value`")
        Input(v-model="route.host.value" :placeholder="route.host.kind === 'ascii_case_insensitive_exact_authority' ? 'API.Example.Test:8443' : route.host.kind === 'exact_authority' ? 'api.example.test:8443' : '*.example.test'")

  ConfigGroup.object-block(path="http_services[].routes[].path" legend="Path matcher")
    .field-grid
      ConfigField(path="http_services[].routes[].path.kind" label="Path match kind")
        Select(
          v-model="route.path.kind"
          :options="[{ value: 'segment_prefix', label: 'Segment prefix' }, { value: 'raw_prefix', label: 'Raw prefix' }, { value: 'exact', label: 'Exact path' }]"
        )
      ConfigField(path="http_services[].routes[].path.value" label="Absolute path")
        Input(v-model="route.path.value" placeholder="/api")
        small Segment prefix respects path boundaries; raw prefix matches bytes directly.

  StringListField(
    v-model="route.methods"
    label="HTTP methods"
    item-label="method"
    field-path="http_services[].routes[].methods"
    hint="An empty list matches every method. The server canonicalizes valid tokens to uppercase."
    :max-items="16"
    @update:model-value="emit('changed')"
  )

  ConfigGroup.object-block(path="http_services[].routes[].access_policy" legend="Access policy")
    Checkbox.enable-row(:model-value="route.access_policy !== null" @change="toggleAccess") Require a bearer token loaded from a server file
    .field-grid(v-if="bearerPolicy")
      ConfigField(path="http_services[].routes[].access_policy.type" label="Policy type")
        Select(
          :model-value="bearerPolicy.type" disabled title="Bearer token file is the only supported route access policy."
          :options="[{ value: 'bearer_token_file', label: 'Bearer token file' }]"
        )
      ConfigField(path="http_services[].routes[].access_policy.token_file_path" label="Token file path")
        Input(v-model="bearerPolicy.token_file_path" autocomplete="off" placeholder="/run/secrets/api-token")
        small Authenticated configuration only; this path is suppressed from topology views.
      ConfigField(path="http_services[].routes[].access_policy.header_name" label="Request header name")
        Input(v-model="bearerPolicy.header_name" placeholder="authorization")
      ConfigField(path="http_services[].routes[].access_policy.realm" label="Bearer realm")
        Input(:model-value="bearerPolicy.realm ?? ''" placeholder="Optional" @input="setRealm")

  ConfigGroup.object-block(path="http_services[].routes[].action" legend="Route action")
    ConfigField(path="http_services[].routes[].action.type" label="Action type")
      Select(
        :model-value="route.action.type" @change="changeAction"
        :options="[{ value: 'proxy', label: 'Proxy upstream' }, { value: 'fixed_response', label: 'Fixed response' }, { value: 'redirect', label: 'Redirect' }, { value: 'static_files', label: 'Static files' }]"
      )

    HttpProxyActionEditor(v-if="route.action.type === 'proxy'" :action="route.action" :route-policy="route.policy" :pool-names="poolNames" :cache-store-names="cacheStoreNames" @changed="emit('changed')")
    HttpFixedResponseEditor(v-else-if="route.action.type === 'fixed_response'" :action="route.action" @changed="emit('changed')")
    HttpRedirectEditor(v-else-if="route.action.type === 'redirect'" :action="route.action")
    HttpStaticFilesEditor(v-else :action="route.action" @changed="emit('changed')")
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import { computed } from 'vue'

import StringListField from '../StringListField.vue'
import type { HttpRouteActionConfig, HttpRouteConfig } from '../config'
import HttpFixedResponseEditor from './HttpFixedResponseEditor.vue'
import HttpProxyActionEditor from './HttpProxyActionEditor.vue'
import HttpRedirectEditor from './HttpRedirectEditor.vue'
import HttpStaticFilesEditor from './HttpStaticFilesEditor.vue'
import { defaultHttpAccessPolicy, defaultHttpAction } from './httpDefaults'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{
  route: HttpRouteConfig
  index: number
  poolNames: string[]
  cacheStoreNames: string[]
}>()

const emit = defineEmits<{
  changed: []
  remove: []
}>()

const routeSummary = computed(() => {
  const host = props.route.host?.value ?? '*'
  const cache = props.route.action.type === 'proxy' && props.route.action.policy.cache
    ? ` / cache ${props.route.action.policy.cache.store || 'unassigned'}`
    : ''
  return `${host} ${props.route.path.value} / ${props.route.action.type.replaceAll('_', ' ')}${cache}`
})

const bearerPolicy = computed(() => (props.route.access_policy?.type === 'bearer_token_file' ? props.route.access_policy : null))

function toggleHost(event: Event): void {
  props.route.host = (event.target as HTMLInputElement).checked
    ? { kind: 'normalized_host', value: '' }
    : null
}

function toggleAccess(event: Event): void {
  props.route.access_policy = (event.target as HTMLInputElement).checked
    ? defaultHttpAccessPolicy()
    : null
}

function setRealm(event: Event): void {
  if (bearerPolicy.value) bearerPolicy.value.realm = (event.target as HTMLInputElement).value || null
}

function changeAction(event: Event): void {
  props.route.action = defaultHttpAction(
    (event.target as HTMLSelectElement).value as HttpRouteActionConfig['type'],
  )
}
</script>
