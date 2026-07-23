<template lang="pug">
fieldset.object-block(data-field="http_services[].routes[].action.policy.cache")
  legend Response cache
  label.enable-row
    input(type="checkbox" :checked="policy.cache !== null" @change="toggleCache")
    span Enable bounded response caching for this route

  template(v-if="cache")
    .field-grid
      label.field(data-field="http_services[].routes[].action.policy.cache.store")
        span Cache store
        select(v-model="cache.store" required)
          option(value="") Select a store
          option(v-for="name in storeNames" :key="name" :value="name") {{ name }}
      label.field(data-field="http_services[].routes[].action.policy.cache.default_ttl_ms")
        span Default TTL (ms)
        input(type="number" min="0" max="31536000000" step="1" v-model.number="cache.default_ttl_ms")
      label.field(data-field="http_services[].routes[].action.policy.cache.grace_ms")
        span Grace period (ms)
        input(type="number" min="0" :max="Math.min(31536000000, cache.keep_ms)" step="1" v-model.number="cache.grace_ms")
      label.field(data-field="http_services[].routes[].action.policy.cache.keep_ms")
        span Keep period (ms)
        input(type="number" :min="cache.grace_ms" max="31536000000" step="1" v-model.number="cache.keep_ms")
      label.field(data-field="http_services[].routes[].action.policy.cache.set_cookie_policy")
        span Set-Cookie response policy
        select(v-model="cache.set_cookie_policy")
          option(value="bypass") Bypass cache
          option(value="ignore") Ignore Set-Cookie
      label.field(data-field="http_services[].routes[].action.policy.cache.authorization_policy")
        span Authorization request policy
        select(v-model="cache.authorization_policy")
          option(value="bypass") Bypass cache
          option(value="cache") Allow caching
      label.field(data-field="http_services[].routes[].action.policy.cache.vary_policy")
        span Vary response policy
        select(v-model="cache.vary_policy")
          option(value="respect") Respect Vary
          option(value="ignore") Ignore Vary
    .field-grid.boolean-grid
      label.enable-row.compact-enable(data-field="http_services[].routes[].action.policy.cache.use_origin_cache_control")
        input(type="checkbox" v-model="cache.use_origin_cache_control")
        span Use origin Cache-Control
      label.enable-row.compact-enable(data-field="http_services[].routes[].action.policy.cache.revalidate")
        input(type="checkbox" v-model="cache.revalidate")
        span Revalidate stale entries
      label.enable-row.compact-enable(data-field="http_services[].routes[].action.policy.cache.collapsed_forwarding")
        input(type="checkbox" v-model="cache.collapsed_forwarding")
        span Collapse concurrent fills

    fieldset.retry-triggers(data-field="http_services[].routes[].action.policy.cache.methods")
      legend Cacheable methods
      label.enable-row(v-for="method in ['GET', 'HEAD']" :key="method")
        input(type="checkbox" :checked="cache.methods.includes(method)" :disabled="cache.methods.length === 1 && cache.methods.includes(method)" @change="toggleValue(cache.methods, method, $event)")
        span {{ method }}

    fieldset.route-list(data-field="http_services[].routes[].action.policy.cache.key_components")
      .route-heading
        legend Cache key components
        button.add-row(type="button" :disabled="cache.key_components.length >= 32" @click="cache.key_components.push({ type: 'scheme' })") + Add component
      article.route-card(v-for="(component, index) in cache.key_components" :key="index")
        header.route-card-heading
          strong Key component {{ index + 1 }}
          button.danger-link(type="button" :disabled="cache.key_components.length === 1" :aria-label="`Remove cache key component ${index + 1}`" @click="cache.key_components.splice(index, 1)") Remove
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.cache.key_components[].type")
            span Component type
            select(:value="component.type" @change="changeKeyComponent(index, $event)")
              option(value="scheme") Scheme
              option(value="normalized_host") Normalized host
              option(value="path_and_query") Path and query
              option(value="header") Request header
              option(value="cookie") Request cookie
          label.field(v-if="component.type === 'header' || component.type === 'cookie'" data-field="http_services[].routes[].action.policy.cache.key_components[].name")
            span {{ component.type === 'header' ? 'Header' : 'Cookie' }} name
            input(type="text" v-model="component.name")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.cache.status_ttls")
      .route-heading
        legend Status TTL overrides
        button.add-row(type="button" :disabled="cache.status_ttls.length >= 64" @click="cache.status_ttls.push({ status: 200, ttl_ms: 60000 })") + Add status TTL
      article.route-card(v-for="(entry, index) in cache.status_ttls" :key="index")
        header.route-card-heading
          strong Status TTL {{ index + 1 }}
          button.danger-link(type="button" :aria-label="`Remove status TTL ${index + 1}`" @click="cache.status_ttls.splice(index, 1)") Remove
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.cache.status_ttls[].status")
            span HTTP status
            input(type="number" min="100" max="599" step="1" v-model.number="entry.status")
          label.field(data-field="http_services[].routes[].action.policy.cache.status_ttls[].ttl_ms")
            span TTL (ms)
            input(type="number" min="0" max="31536000000" step="1" v-model.number="entry.ttl_ms")

    fieldset.retry-triggers(data-field="http_services[].routes[].action.policy.cache.stale_on")
      legend Serve stale on
      label.enable-row(v-for="trigger in CACHE_STALE_TRIGGERS" :key="trigger")
        input(type="checkbox" :checked="cache.stale_on.includes(trigger)" @change="toggleValue(cache.stale_on, trigger, $event)")
        span {{ trigger.replaceAll('_', ' ') }}

    fieldset.route-list(data-field="http_services[].routes[].action.policy.cache.bypass_request")
      .route-heading
        legend Bypass request predicates
        button.add-row(type="button" :disabled="cache.bypass_request.length >= 32" @click="addPredicate(cache.bypass_request)") + Add predicate
      CachePredicateRows(v-model="cache.bypass_request" type-field="http_services[].routes[].action.policy.cache.bypass_request[].type" name-field="http_services[].routes[].action.policy.cache.bypass_request[].name")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.cache.no_store_request")
      .route-heading
        legend No-store request predicates
        button.add-row(type="button" :disabled="cache.no_store_request.length >= 32" @click="addPredicate(cache.no_store_request)") + Add predicate
      CachePredicateRows(v-model="cache.no_store_request" type-field="http_services[].routes[].action.policy.cache.no_store_request[].type" name-field="http_services[].routes[].action.policy.cache.no_store_request[].name")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.cache.no_store_response")
      .route-heading
        legend No-store response predicates
        button.add-row(type="button" :disabled="cache.no_store_response.length >= 32" @click="addPredicate(cache.no_store_response)") + Add predicate
      CachePredicateRows(v-model="cache.no_store_response" type-field="http_services[].routes[].action.policy.cache.no_store_response[].type" name-field="http_services[].routes[].action.policy.cache.no_store_response[].name")

    fieldset.object-block(data-field="http_services[].routes[].action.policy.cache.surrogate_tags")
      legend Surrogate tags
      label.enable-row
        input(type="checkbox" :checked="cache.surrogate_tags !== null" @change="toggleSurrogateTags")
        span Read bounded surrogate tags from an origin response header
      .field-grid(v-if="cache.surrogate_tags")
        label.field(data-field="http_services[].routes[].action.policy.cache.surrogate_tags.response_header")
          span Response header
          input(type="text" v-model="cache.surrogate_tags.response_header")
        label.field(data-field="http_services[].routes[].action.policy.cache.surrogate_tags.max_tags")
          span Maximum tags
          input(type="number" min="1" max="256" step="1" v-model.number="cache.surrogate_tags.max_tags")
        label.field(data-field="http_services[].routes[].action.policy.cache.surrogate_tags.max_tag_bytes")
          span Maximum tag bytes
          input(type="number" min="1" max="1024" step="1" v-model.number="cache.surrogate_tags.max_tag_bytes")

    fieldset.object-block(data-field="http_services[].routes[].action.policy.cache.purge_authorization")
      legend Purge authorization
      label.enable-row
        input(type="checkbox" :checked="cache.purge_authorization !== null" @change="togglePurgeAuthorization")
        span Require a bearer token loaded from a server file
      .field-grid(v-if="cache.purge_authorization")
        label.field(data-field="http_services[].routes[].action.policy.cache.purge_authorization.type")
          span Authorization type
          select(v-model="cache.purge_authorization.type" disabled)
            option(value="bearer_token_file") Bearer token file
        label.field(data-field="http_services[].routes[].action.policy.cache.purge_authorization.token_file_path")
          span Token file path
          input(type="text" v-model="cache.purge_authorization.token_file_path" autocomplete="off")
          small Authenticated configuration only; this path is suppressed from topology views.
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type {
  CacheKeyComponentConfig,
  CachePredicateConfig,
  CacheStaleTrigger,
  HttpProxyPolicyConfig,
} from '../config'
import CachePredicateRows from './CachePredicateRows.vue'
import { CACHE_STALE_TRIGGERS, defaultHttpCachePolicy } from './canonicalDefaults'

const props = defineProps<{
  policy: HttpProxyPolicyConfig
  storeNames: string[]
}>()
const cache = computed(() => props.policy.cache)

function toggleCache(event: Event): void {
  props.policy.cache = (event.target as HTMLInputElement).checked
    ? defaultHttpCachePolicy(props.storeNames[0] ?? '')
    : null
}

function changeKeyComponent(index: number, event: Event): void {
  if (!props.policy.cache) return
  const type = (event.target as HTMLSelectElement).value as CacheKeyComponentConfig['type']
  props.policy.cache.key_components[index] = type === 'header' || type === 'cookie'
    ? { type, name: '' }
    : { type }
}

function addPredicate(predicates: CachePredicateConfig[]): void {
  if (predicates.length < 32) predicates.push({ type: 'header_present', name: '' })
}

function toggleValue<T extends string>(values: T[], value: T, event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!values.includes(value)) values.push(value)
  } else if (values.length > 1 || values !== props.policy.cache?.methods) {
    const index = values.indexOf(value)
    if (index >= 0) values.splice(index, 1)
  }
}

function toggleSurrogateTags(event: Event): void {
  if (!props.policy.cache) return
  props.policy.cache.surrogate_tags = (event.target as HTMLInputElement).checked
    ? { response_header: 'surrogate-key', max_tags: 64, max_tag_bytes: 256 }
    : null
}

function togglePurgeAuthorization(event: Event): void {
  if (!props.policy.cache) return
  props.policy.cache.purge_authorization = (event.target as HTMLInputElement).checked
    ? { type: 'bearer_token_file', token_file_path: '' }
    : null
}
</script>
