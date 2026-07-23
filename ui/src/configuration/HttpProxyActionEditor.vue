<template lang="pug">
.action-editor
  label.field(data-field="http_services[].routes[].action.upstream_pool")
    span Upstream pool
    select(v-model="action.upstream_pool" required)
      option(value="") Select a pool
      option(v-for="name in poolNames" :key="name" :value="name") {{ name }}

  fieldset.object-block(data-field="http_services[].routes[].action.policy")
    legend Proxy policy
    fieldset.object-block(data-field="http_services[].routes[].action.policy.upstream_host")
      legend Upstream Host header
      .field-grid
        label.field(data-field="http_services[].routes[].action.policy.upstream_host.type")
          span Host policy
          select(:value="action.policy.upstream_host.type" @change="changeUpstreamHost")
            option(value="preserve_incoming") Preserve incoming authority
            option(value="endpoint") Selected endpoint authority
            option(value="literal") Literal authority
        label.field(v-if="action.policy.upstream_host.type === 'endpoint'" data-field="http_services[].routes[].action.policy.upstream_host.unix_fallback")
          span Unix endpoint fallback
          input(type="text" :value="action.policy.upstream_host.unix_fallback ?? ''" placeholder="localhost" @input="setUnixFallback")
          small Required when the selected pool contains a Unix endpoint.
        label.field(v-else-if="action.policy.upstream_host.type === 'literal'" data-field="http_services[].routes[].action.policy.upstream_host.value")
          span Literal authority
          input(type="text" v-model="action.policy.upstream_host.value" placeholder="origin.example.test")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.request_headers")
      .route-heading
        legend Request header mutations
        button.add-row(type="button" :disabled="action.policy.request_headers.length >= 32" :title="action.policy.request_headers.length >= 32 ? 'The server allows at most 32 request header mutations.' : undefined" @click="addHeader(action.policy.request_headers)") + Add request mutation
      article.route-card(v-for="(mutation, mutationIndex) in action.policy.request_headers" :key="mutationIndex")
        header.route-card-heading
          strong Request mutation {{ mutationIndex + 1 }}
          button.danger-link(type="button" :aria-label="`Remove request header mutation ${mutationIndex + 1}`" @click="removeHeader(action.policy.request_headers, mutationIndex)") Remove
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.request_headers[].operation")
            span Operation
            select(:value="mutation.operation" @change="changeRequestOperation(mutationIndex, $event)")
              option(value="set") Set
              option(value="remove") Remove
          label.field(data-field="http_services[].routes[].action.policy.request_headers[].name")
            span Header name
            input(type="text" v-model="mutation.name" placeholder="x-forwarded-for")
          fieldset.object-block(v-if="mutation.operation === 'set'" data-field="http_services[].routes[].action.policy.request_headers[].value")
            legend Value source
            .field-grid
              label.field(data-field="http_services[].routes[].action.policy.request_headers[].value.type")
                span Source type
                select(:value="mutation.value.type" @change="changeRequestValue(mutationIndex, $event)")
                  option(value="literal") Literal
                  option(value="incoming_authority") Incoming authority
                  option(value="normalized_host") Normalized host
                  option(value="client_ip") Client IP
                  option(value="selected_upstream_host") Selected upstream Host
              label.field(v-if="mutation.value.type === 'literal'" data-field="http_services[].routes[].action.policy.request_headers[].value.value")
                span Literal value
                input(type="text" v-model="mutation.value.value")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.response_headers")
      .route-heading
        legend Response header mutations
        button.add-row(type="button" :disabled="action.policy.response_headers.length >= 32" :title="action.policy.response_headers.length >= 32 ? 'The server allows at most 32 response header mutations.' : undefined" @click="addHeader(action.policy.response_headers)") + Add response mutation
      article.route-card(v-for="(mutation, mutationIndex) in action.policy.response_headers" :key="mutationIndex")
        header.route-card-heading
          strong Response mutation {{ mutationIndex + 1 }}
          button.danger-link(type="button" :aria-label="`Remove response header mutation ${mutationIndex + 1}`" @click="removeHeader(action.policy.response_headers, mutationIndex)") Remove
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.response_headers[].operation")
            span Operation
            select(:value="mutation.operation" @change="changeResponseOperation(mutationIndex, $event)")
              option(value="set") Set
              option(value="remove") Remove
          label.field(data-field="http_services[].routes[].action.policy.response_headers[].name")
            span Header name
            input(type="text" v-model="mutation.name")
          label.field(v-if="mutation.operation === 'set'" data-field="http_services[].routes[].action.policy.response_headers[].value")
            span Header value
            input(type="text" v-model="mutation.value")

    fieldset.route-list(data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites")
      .route-heading
        legend Response cookie path rewrites
        button.add-row(type="button" :disabled="action.policy.response_cookie_path_rewrites.length >= 16" :title="action.policy.response_cookie_path_rewrites.length >= 16 ? 'The server allows at most 16 cookie path rewrites.' : undefined" @click="addCookieRewrite") + Add cookie rewrite
      article.route-card(v-for="(rewrite, rewriteIndex) in action.policy.response_cookie_path_rewrites" :key="rewriteIndex")
        header.route-card-heading
          strong Cookie rewrite {{ rewriteIndex + 1 }}
          button.danger-link(type="button" :aria-label="`Remove cookie path rewrite ${rewriteIndex + 1}`" @click="removeCookieRewrite(rewriteIndex)") Remove
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].from")
            span Source path
            input(type="text" v-model="rewrite.from" placeholder="/")
          label.field(data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].to")
            span Replacement path
            input(type="text" v-model="rewrite.to" placeholder="/app")

    fieldset.object-block(data-field="http_services[].routes[].action.policy.retry")
      legend Retry policy
      .field-grid
        label.field(data-field="http_services[].routes[].action.policy.retry.max_retries")
          span Maximum retries
          input(type="number" min="0" max="2" step="1" v-model.number="action.policy.retry.max_retries")
        label.field(data-field="http_services[].routes[].action.policy.retry.method_safety")
          span Method safety
          select(v-model="action.policy.retry.method_safety" disabled title="Retries are restricted to GET and HEAD.")
            option(value="get_head") GET and HEAD only
        label.field(data-field="http_services[].routes[].action.policy.retry.body_safety")
          span Body safety
          select(v-model="action.policy.retry.body_safety" disabled title="Retries require an empty request body.")
            option(value="empty") Empty body only
      fieldset.retry-triggers(data-field="http_services[].routes[].action.policy.retry.triggers")
        legend Retry triggers
        label.enable-row(v-for="trigger in HTTP_RETRY_TRIGGERS" :key="trigger")
          input(
            type="checkbox"
            :checked="action.policy.retry.triggers.includes(trigger)"
            :disabled="action.policy.retry.triggers.length === 1 && action.policy.retry.triggers.includes(trigger)"
            :title="action.policy.retry.triggers.length === 1 && action.policy.retry.triggers.includes(trigger) ? 'The server requires at least one retry trigger.' : undefined"
            @change="toggleRetryTrigger(trigger, $event)"
          )
          span {{ retryTriggerLabels[trigger] }}

    HttpCachePolicyEditor(:policy="action.policy" :store-names="cacheStoreNames")
</template>

<script setup lang="ts">
import type {
  HttpProxyActionConfig,
  HttpRequestHeaderValueConfig,
  HttpRetryTrigger,
} from '../config'
import { HTTP_RETRY_TRIGGERS, defaultUpstreamHost } from './httpDefaults'
import HttpCachePolicyEditor from './HttpCachePolicyEditor.vue'

const props = defineProps<{
  action: HttpProxyActionConfig
  poolNames: string[]
  cacheStoreNames: string[]
}>()
const emit = defineEmits<{ changed: [] }>()

const retryTriggerLabels: Record<HttpRetryTrigger, string> = {
  connect_failure: 'Connection failure',
  connect_timeout: 'Connection timeout',
  refused_stream: 'HTTP/2 refused stream',
}

function changeUpstreamHost(event: Event): void {
  props.action.policy.upstream_host = defaultUpstreamHost(
    (event.target as HTMLSelectElement).value as HttpProxyActionConfig['policy']['upstream_host']['type'],
  )
}

function setUnixFallback(event: Event): void {
  if (props.action.policy.upstream_host.type === 'endpoint') {
    props.action.policy.upstream_host.unix_fallback = (event.target as HTMLInputElement).value || null
  }
}

function changeRequestOperation(index: number, event: Event): void {
  changeHeaderOperation(
    props.action.policy.request_headers,
    index,
    event,
    { type: 'literal', value: '' },
  )
}

function changeRequestValue(index: number, event: Event): void {
  const current = props.action.policy.request_headers[index]
  if (!current || current.operation !== 'set') return
  const type = (event.target as HTMLSelectElement).value as HttpRequestHeaderValueConfig['type']
  current.value = type === 'literal' ? { type, value: '' } : { type }
}

function changeResponseOperation(index: number, event: Event): void {
  changeHeaderOperation(props.action.policy.response_headers, index, event, '')
}

type HeaderMutation<T> =
  | { operation: 'set'; name: string; value: T }
  | { operation: 'remove'; name: string }

function addHeader<T>(headers: HeaderMutation<T>[]): void {
  if (headers.length >= 32) return
  headers.push({ operation: 'remove', name: '' })
  emit('changed')
}

function removeHeader<T>(headers: HeaderMutation<T>[], index: number): void {
  headers.splice(index, 1)
  emit('changed')
}

function changeHeaderOperation<T>(
  headers: HeaderMutation<T>[],
  index: number,
  event: Event,
  value: T,
): void {
  const current = headers[index]
  if (!current) return
  headers[index] = (event.target as HTMLSelectElement).value === 'set'
    ? { operation: 'set', name: current.name, value }
    : { operation: 'remove', name: current.name }
}

function addCookieRewrite(): void {
  if (props.action.policy.response_cookie_path_rewrites.length >= 16) return
  props.action.policy.response_cookie_path_rewrites.push({ from: '/', to: '/' })
  emit('changed')
}

function removeCookieRewrite(index: number): void {
  props.action.policy.response_cookie_path_rewrites.splice(index, 1)
  emit('changed')
}

function toggleRetryTrigger(trigger: HttpRetryTrigger, event: Event): void {
  const triggers = props.action.policy.retry.triggers
  if ((event.target as HTMLInputElement).checked) {
    if (!triggers.includes(trigger)) triggers.push(trigger)
  } else if (triggers.length > 1) {
    props.action.policy.retry.triggers = triggers.filter((candidate) => candidate !== trigger)
  }
}
</script>
