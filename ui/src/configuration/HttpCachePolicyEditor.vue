<template lang="pug">
ConfigGroup.object-block(:path="fieldPath" :legend="fieldPath.startsWith('forward_proxy_services') ? 'Forward response cache' : 'Response cache'")
  label.enable-row
    input(type="checkbox" :checked="cache !== null" @change="toggleCache")
    span {{ fieldPath.startsWith('forward_proxy_services') ? 'Enable bounded response caching for this forward service' : 'Enable bounded response caching for this route' }}

  template(v-if="cache")
    .field-grid
      ConfigField(:path="`${fieldPath}.store`" label="Cache store")
        select(v-model="cache.store" required)
          option(value="") Select a store
          option(v-for="name in storeNames" :key="name" :value="name") {{ name }}
      ConfigField(:path="`${fieldPath}.default_ttl_ms`" label="Default TTL (ms)")
        input(type="number" min="0" max="31536000000" step="1" v-model.number="cache.default_ttl_ms")
      ConfigField(:path="`${fieldPath}.grace_ms`" label="Grace period (ms)")
        input(type="number" min="0" :max="Math.min(31536000000, cache.keep_ms)" step="1" v-model.number="cache.grace_ms")
      ConfigField(:path="`${fieldPath}.keep_ms`" label="Keep period (ms)")
        input(type="number" :min="cache.grace_ms" max="31536000000" step="1" v-model.number="cache.keep_ms")
      ConfigField(:path="`${fieldPath}.set_cookie_policy`" label="Set-Cookie response policy")
        select(v-model="cache.set_cookie_policy")
          option(value="bypass") Bypass cache
          option(value="ignore") Ignore Set-Cookie
      ConfigField(:path="`${fieldPath}.authorization_policy`" label="Authorization request policy")
        select(v-model="cache.authorization_policy")
          option(value="bypass") Bypass cache
          option(value="cache") Allow caching
      ConfigField(:path="`${fieldPath}.vary_policy`" label="Vary response policy")
        select(v-model="cache.vary_policy")
          option(value="respect") Respect Vary
          option(value="ignore") Ignore Vary
    .field-grid.boolean-grid
      ConfigField.compact-enable(:path="`${fieldPath}.use_origin_cache_control`" label="Use origin Cache-Control" check)
        input(type="checkbox" v-model="cache.use_origin_cache_control")
      ConfigField.compact-enable(:path="`${fieldPath}.revalidate`" label="Revalidate stale entries" check)
        input(type="checkbox" v-model="cache.revalidate")
      ConfigField.compact-enable(:path="`${fieldPath}.collapsed_forwarding`" label="Collapse concurrent fills" check)
        input(type="checkbox" v-model="cache.collapsed_forwarding")

    ConfigGroup.retry-triggers(:path="`${fieldPath}.methods`" legend="Cacheable methods")
      label.enable-row(v-for="method in ['GET', 'HEAD']" :key="method")
        input(type="checkbox" :checked="cache.methods.includes(method)" :disabled="cache.methods.length === 1 && cache.methods.includes(method)" @change="toggleValue(cache.methods, method, $event)")
        span {{ method }}

    ConfigGroup.route-list(:path="`${fieldPath}.key_components`")
      template(#legend)
        .route-heading
          legend Cache key components
          button.add-row(type="button" :disabled="cache.key_components.length >= 32" @click="cache.key_components.push({ type: 'scheme' })") + Add component
      ItemScope.route-card(v-for="(component, index) in cache.key_components" :key="index" :index="index")
        header.route-card-heading
          strong Key component {{ index + 1 }}
          button.danger-link(type="button" :disabled="cache.key_components.length === 1" :aria-label="`Remove cache key component ${index + 1}`" @click="cache.key_components.splice(index, 1)") Remove
        .field-grid
          ConfigField(:path="`${fieldPath}.key_components[].type`" label="Component type")
            select(:value="component.type" @change="changeKeyComponent(index, $event)")
              option(value="scheme") Scheme
              option(value="normalized_host") Normalized host
              option(value="path_and_query") Path and query
              option(value="header") Request header
              option(value="cookie") Request cookie
          ConfigField(v-if="component.type === 'header' || component.type === 'cookie'" :path="`${fieldPath}.key_components[].name`" :label="`${component.type === 'header' ? 'Header' : 'Cookie'} name`")
            input(type="text" v-model="component.name")

    ConfigGroup.route-list(:path="`${fieldPath}.status_ttls`")
      template(#legend)
        .route-heading
          legend Status TTL overrides
          button.add-row(type="button" :disabled="cache.status_ttls.length >= 64" @click="cache.status_ttls.push({ status: 200, ttl_ms: 60000 })") + Add status TTL
      ItemScope.route-card(v-for="(entry, index) in cache.status_ttls" :key="index" :index="index")
        header.route-card-heading
          strong Status TTL {{ index + 1 }}
          button.danger-link(type="button" :aria-label="`Remove status TTL ${index + 1}`" @click="cache.status_ttls.splice(index, 1)") Remove
        .field-grid
          ConfigField(:path="`${fieldPath}.status_ttls[].status`" label="HTTP status")
            input(type="number" min="100" max="599" step="1" v-model.number="entry.status")
          ConfigField(:path="`${fieldPath}.status_ttls[].ttl_ms`" label="TTL (ms)")
            input(type="number" min="0" max="31536000000" step="1" v-model.number="entry.ttl_ms")

    ConfigGroup.retry-triggers(:path="`${fieldPath}.stale_on`" legend="Serve stale on")
      label.enable-row(v-for="trigger in CACHE_STALE_TRIGGERS" :key="trigger")
        input(type="checkbox" :checked="cache.stale_on.includes(trigger)" @change="toggleValue(cache.stale_on, trigger, $event)")
        span {{ trigger.replaceAll('_', ' ') }}

    ConfigGroup.route-list(:path="`${fieldPath}.bypass_request`")
      template(#legend)
        .route-heading
          legend Bypass request predicates
          button.add-row(type="button" :disabled="cache.bypass_request.length >= 32" @click="addPredicate(cache.bypass_request)") + Add predicate
      CachePredicateRows(v-model="cache.bypass_request" :type-field="`${fieldPath}.bypass_request[].type`" :name-field="`${fieldPath}.bypass_request[].name`")

    ConfigGroup.route-list(:path="`${fieldPath}.no_store_request`")
      template(#legend)
        .route-heading
          legend No-store request predicates
          button.add-row(type="button" :disabled="cache.no_store_request.length >= 32" @click="addPredicate(cache.no_store_request)") + Add predicate
      CachePredicateRows(v-model="cache.no_store_request" :type-field="`${fieldPath}.no_store_request[].type`" :name-field="`${fieldPath}.no_store_request[].name`")

    ConfigGroup.route-list(:path="`${fieldPath}.no_store_response`")
      template(#legend)
        .route-heading
          legend No-store response predicates
          button.add-row(type="button" :disabled="cache.no_store_response.length >= 32" @click="addPredicate(cache.no_store_response)") + Add predicate
      CachePredicateRows(v-model="cache.no_store_response" :type-field="`${fieldPath}.no_store_response[].type`" :name-field="`${fieldPath}.no_store_response[].name`")

    ConfigGroup.object-block(:path="`${fieldPath}.surrogate_tags`" legend="Surrogate tags")
      label.enable-row
        input(type="checkbox" :checked="cache.surrogate_tags !== null" @change="toggleSurrogateTags")
        span Read bounded surrogate tags from an origin response header
      .field-grid(v-if="cache.surrogate_tags")
        ConfigField(:path="`${fieldPath}.surrogate_tags.response_header`" label="Response header")
          input(type="text" v-model="cache.surrogate_tags.response_header")
        ConfigField(:path="`${fieldPath}.surrogate_tags.max_tags`" label="Maximum tags")
          input(type="number" min="1" max="256" step="1" v-model.number="cache.surrogate_tags.max_tags")
        ConfigField(:path="`${fieldPath}.surrogate_tags.max_tag_bytes`" label="Maximum tag bytes")
          input(type="number" min="1" max="1024" step="1" v-model.number="cache.surrogate_tags.max_tag_bytes")

    ConfigGroup.object-block(:path="`${fieldPath}.purge_authorization`" legend="Purge authorization")
      label.enable-row
        input(type="checkbox" :checked="cache.purge_authorization !== null" @change="togglePurgeAuthorization")
        span Require a bearer token loaded from a server file
      .field-grid(v-if="cache.purge_authorization")
        ConfigField(:path="`${fieldPath}.purge_authorization.type`" label="Authorization type")
          select(v-model="cache.purge_authorization.type" disabled)
            option(value="bearer_token_file") Bearer token file
        ConfigField(:path="`${fieldPath}.purge_authorization.token_file_path`" label="Token file path")
          input(type="text" v-model="cache.purge_authorization.token_file_path" autocomplete="off")
          small Authenticated configuration only; this path is suppressed from topology views.

    ConfigGroup.object-block(:path="`${fieldPath}.compression`" legend="Compressed variants")
      label.enable-row
        input(type="checkbox" :checked="!!cache.compression" @change="toggleCompression")
        span Store one compressed copy of each compressible response per negotiated coding
      template(v-if="cache.compression")
        ConfigGroup.retry-triggers(:path="`${fieldPath}.compression.codings`" legend="Offered codings, most preferred first")
          label.enable-row(v-for="coding in CACHE_COMPRESSION_CODINGS" :key="coding")
            input(type="checkbox" :checked="cache.compression.codings.includes(coding)" @change="toggleCoding(cache.compression, coding, $event)")
            span {{ coding }}
        ConfigGroup.retry-triggers(:path="`${fieldPath}.compression.precompress`" legend="Compress when the response is stored")
          label.enable-row(v-for="coding in cache.compression.codings" :key="coding")
            input(type="checkbox" :checked="cache.compression.precompress.includes(coding)" @change="toggleValue(cache.compression.precompress, coding, $event)")
            span {{ coding }}
        .field-grid
          ConfigField(:path="`${fieldPath}.compression.content_types`" label="Content types, one per line")
            textarea(:value="contentTypeLines(cache.compression)" rows="4" @input="setContentTypes")
          ConfigField(:path="`${fieldPath}.compression.min_length_bytes`" label="Minimum body bytes")
            input(type="number" min="0" step="1" v-model.number="cache.compression.min_length_bytes")
          ConfigField(:path="`${fieldPath}.compression.gzip_level`" label="Gzip level")
            input(type="number" min="1" max="12" step="1" v-model.number="cache.compression.gzip_level")
          ConfigField(:path="`${fieldPath}.compression.brotli_level`" label="Brotli level")
            input(type="number" min="0" max="11" step="1" v-model.number="cache.compression.brotli_level")
          ConfigField(:path="`${fieldPath}.compression.zstd_level`" label="Zstandard level")
            input(type="number" min="-7" max="19" step="1" v-model.number="cache.compression.zstd_level")
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import type {
  CacheCompressionConfig,
  CacheCompressionCoding,
  CacheKeyComponentConfig,
  CachePredicateConfig,
  CacheStaleTrigger,
  HttpCachePolicyConfig,
} from '../config'
import CachePredicateRows from './CachePredicateRows.vue'
import {
  CACHE_COMPRESSION_CODINGS,
  CACHE_STALE_TRIGGERS,
  defaultCacheCompression,
  defaultHttpCachePolicy,
} from './canonicalDefaults'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{
  modelValue: HttpCachePolicyConfig | null | undefined
  fieldPath: string
  storeNames: string[]
}>()
const emit = defineEmits<{ 'update:modelValue': [value: HttpCachePolicyConfig | null] }>()
const cache = computed(() => props.modelValue ?? null)

function toggleCache(event: Event): void {
  emit(
    'update:modelValue',
    (event.target as HTMLInputElement).checked
      ? defaultHttpCachePolicy(props.storeNames[0] ?? '')
      : null,
  )
}

function changeKeyComponent(index: number, event: Event): void {
  if (!props.modelValue) return
  const type = (event.target as HTMLSelectElement).value as CacheKeyComponentConfig['type']
  props.modelValue.key_components[index] = type === 'header' || type === 'cookie'
    ? { type, name: '' }
    : { type }
}

function addPredicate(predicates: CachePredicateConfig[]): void {
  if (predicates.length < 32) predicates.push({ type: 'header_present', name: '' })
}

function toggleValue<T extends string>(values: T[], value: T, event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!values.includes(value)) values.push(value)
  } else if (values.length > 1 || values !== props.modelValue?.methods) {
    const index = values.indexOf(value)
    if (index >= 0) values.splice(index, 1)
  }
}

function toggleSurrogateTags(event: Event): void {
  if (!props.modelValue) return
  props.modelValue.surrogate_tags = (event.target as HTMLInputElement).checked
    ? { response_header: 'surrogate-key', max_tags: 64, max_tag_bytes: 256 }
    : null
}

function toggleCompression(event: Event): void {
  if (!props.modelValue) return
  props.modelValue.compression = (event.target as HTMLInputElement).checked
    ? defaultCacheCompression()
    : null
}

function toggleCoding(
  compression: CacheCompressionConfig,
  coding: CacheCompressionCoding,
  event: Event,
): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!compression.codings.includes(coding)) compression.codings.push(coding)
  } else if (compression.codings.length > 1) {
    compression.codings = compression.codings.filter((offered) => offered !== coding)
    compression.precompress = compression.precompress.filter((offered) => offered !== coding)
  }
}

function contentTypeLines(compression: CacheCompressionConfig): string {
  return compression.content_types.join('\n')
}

function setContentTypes(event: Event): void {
  const compression = props.modelValue?.compression
  if (!compression) return
  compression.content_types = (event.target as HTMLTextAreaElement).value
    .split('\n')
    .map((value) => value.trim())
    .filter((value) => value !== '')
}

function togglePurgeAuthorization(event: Event): void {
  if (!props.modelValue) return
  props.modelValue.purge_authorization = (event.target as HTMLInputElement).checked
    ? { type: 'bearer_token_file', token_file_path: '' }
    : null
}
</script>
