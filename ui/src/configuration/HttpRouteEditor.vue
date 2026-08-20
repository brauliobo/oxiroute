<template lang="pug">
article.route-card.http-route-card
  header.route-card-heading
    div
      strong Route {{ index + 1 }}
      span.route-summary {{ routeSummary }}
    button.danger-link(type="button" :aria-label="`Remove route ${index + 1}`" @click="$emit('remove')") Remove

  fieldset.object-block(data-field="http_services[].routes[].host")
    legend Host matcher
    label.enable-row
      input(type="checkbox" :checked="route.host !== null" @change="toggleHost")
      span Match a specific host or authority
    .field-grid(v-if="route.host")
      label.field(data-field="http_services[].routes[].host.kind")
        span Host match kind
        select(v-model="route.host.kind")
          option(value="normalized_host") Normalized host
          option(value="exact_authority") Exact authority
          option(value="ascii_case_insensitive_exact_authority") ASCII case-insensitive exact authority
      label.field(data-field="http_services[].routes[].host.value")
        span {{ route.host.kind.includes('authority') ? 'Authority' : 'Host' }} value
        input(type="text" v-model="route.host.value" :placeholder="route.host.kind === 'ascii_case_insensitive_exact_authority' ? 'API.Example.Test:8443' : route.host.kind === 'exact_authority' ? 'api.example.test:8443' : '*.example.test'")

  fieldset.object-block(data-field="http_services[].routes[].path")
    legend Path matcher
    .field-grid
      label.field(data-field="http_services[].routes[].path.kind")
        span Path match kind
        select(v-model="route.path.kind")
          option(value="segment_prefix") Segment prefix
          option(value="raw_prefix") Raw prefix
          option(value="exact") Exact path
      label.field(data-field="http_services[].routes[].path.value")
        span Absolute path
        input(type="text" v-model="route.path.value" placeholder="/api")
        small Segment prefix respects path boundaries; raw prefix matches bytes directly.

  StringListField(
    v-model="route.methods"
    label="HTTP methods"
    item-label="method"
    field-path="http_services[].routes[].methods"
    hint="An empty list matches every method. The server canonicalizes valid tokens to uppercase."
    :max-items="16"
    @update:model-value="$emit('changed')"
  )

  fieldset.object-block(data-field="http_services[].routes[].access_policy")
    legend Access policy
    label.enable-row
      input(type="checkbox" :checked="route.access_policy !== null" @change="toggleAccess")
      span Require a bearer token loaded from a server file
    .field-grid(v-if="route.access_policy")
      label.field(data-field="http_services[].routes[].access_policy.type")
        span Policy type
        select(v-model="route.access_policy.type" disabled title="Bearer token file is the only supported route access policy.")
          option(value="bearer_token_file") Bearer token file
      label.field(data-field="http_services[].routes[].access_policy.token_file_path")
        span Token file path
        input(type="text" v-model="route.access_policy.token_file_path" autocomplete="off" placeholder="/run/secrets/api-token")
        small Authenticated configuration only; this path is suppressed from topology views.
      label.field(data-field="http_services[].routes[].access_policy.header_name")
        span Request header name
        input(type="text" v-model="route.access_policy.header_name" placeholder="authorization")
      label.field(data-field="http_services[].routes[].access_policy.realm")
        span Bearer realm
        input(type="text" :value="route.access_policy.realm ?? ''" placeholder="Optional" @input="setRealm")

  fieldset.object-block(data-field="http_services[].routes[].action")
    legend Route action
    label.field(data-field="http_services[].routes[].action.type")
      span Action type
      select(:value="route.action.type" @change="changeAction")
        option(value="proxy") Proxy upstream
        option(value="fixed_response") Fixed response
        option(value="redirect") Redirect
        option(value="static_files") Static files

    HttpProxyActionEditor(v-if="route.action.type === 'proxy'" :action="route.action" :route-policy="route.policy" :pool-names="poolNames" :cache-store-names="cacheStoreNames" @changed="$emit('changed')")
    HttpFixedResponseEditor(v-else-if="route.action.type === 'fixed_response'" :action="route.action" @changed="$emit('changed')")
    HttpRedirectEditor(v-else-if="route.action.type === 'redirect'" :action="route.action")
    HttpStaticFilesEditor(v-else :action="route.action" @changed="$emit('changed')")
</template>

<script setup lang="ts">
import { computed } from 'vue'

import StringListField from '../StringListField.vue'
import type { HttpRouteActionConfig, HttpRouteConfig } from '../config'
import HttpFixedResponseEditor from './HttpFixedResponseEditor.vue'
import HttpProxyActionEditor from './HttpProxyActionEditor.vue'
import HttpRedirectEditor from './HttpRedirectEditor.vue'
import HttpStaticFilesEditor from './HttpStaticFilesEditor.vue'
import { defaultHttpAccessPolicy, defaultHttpAction } from './httpDefaults'

const props = defineProps<{
  route: HttpRouteConfig
  index: number
  poolNames: string[]
  cacheStoreNames: string[]
}>()

defineEmits<{
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
  if (props.route.access_policy) {
    props.route.access_policy.realm = (event.target as HTMLInputElement).value || null
  }
}

function changeAction(event: Event): void {
  props.route.action = defaultHttpAction(
    (event.target as HTMLSelectElement).value as HttpRouteActionConfig['type'],
  )
}
</script>
