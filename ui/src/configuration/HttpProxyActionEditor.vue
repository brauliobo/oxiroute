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
            option(value="nginx_host") nginx $host with fallback
            option(value="endpoint") Selected endpoint authority
            option(value="literal") Literal authority
        label.field(v-if="action.policy.upstream_host.type === 'endpoint'" data-field="http_services[].routes[].action.policy.upstream_host.unix_fallback")
          span Unix endpoint fallback
          input(type="text" :value="action.policy.upstream_host.unix_fallback ?? ''" placeholder="localhost" @input="setUnixFallback")
          small Required when the selected pool contains a Unix endpoint.
        label.field(v-else-if="action.policy.upstream_host.type === 'nginx_host'" data-field="http_services[].routes[].action.policy.upstream_host.fallback")
          span Missing-Host fallback
          input(type="text" v-model="action.policy.upstream_host.fallback" placeholder="localhost")
        label.field(v-else-if="action.policy.upstream_host.type === 'literal'" data-field="http_services[].routes[].action.policy.upstream_host.value")
          span Literal authority
          input(type="text" v-model="action.policy.upstream_host.value" placeholder="origin.example.test")

    fieldset.object-block(data-field="http_services[].routes[].action.policy.upstream_path_rewrite")
      legend Upstream path rewrite
      label.enable-row
        input(type="checkbox" :checked="action.policy.upstream_path_rewrite !== null" @change="toggleUpstreamPathRewrite")
        span Rewrite the matched path before proxying
      .field-grid(v-if="action.policy.upstream_path_rewrite")
        label.field(data-field="http_services[].routes[].action.policy.upstream_path_rewrite.from")
          span Source prefix
          input(type="text" v-model="action.policy.upstream_path_rewrite.from" placeholder="/public")
        label.field(data-field="http_services[].routes[].action.policy.upstream_path_rewrite.to")
          span Destination prefix
          input(type="text" v-model="action.policy.upstream_path_rewrite.to" placeholder="/internal")

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
              option(value="add") Add
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
                  option(value="nginx_host") nginx $host with fallback
                  option(value="client_ip") Client IP
                  option(value="appended_x_forwarded_for") Append client IP to X-Forwarded-For
                  option(value="downstream_scheme") Downstream scheme
                  option(value="selected_upstream_host") Selected upstream Host
              label.field(v-if="mutation.value.type === 'literal'" data-field="http_services[].routes[].action.policy.request_headers[].value.value")
                span Literal value
                input(type="text" v-model="mutation.value.value")
              label.field(v-if="mutation.value.type === 'nginx_host'" data-field="http_services[].routes[].action.policy.request_headers[].value.fallback")
                span Missing-Host fallback
                input(type="text" v-model="mutation.value.fallback" placeholder="localhost")
              label.field(v-if="mutation.value.type === 'appended_x_forwarded_for'" data-field="http_services[].routes[].action.policy.request_headers[].value.max_bytes")
                span Maximum header bytes
                input(type="number" min="1" step="1" v-model.number="mutation.value.max_bytes")
            fieldset.route-list(v-if="mutation.value.type === 'appended_x_forwarded_for'" data-field="http_services[].routes[].action.policy.request_headers[].value.except_source_cidrs")
              .route-heading
                legend Source CIDR exceptions
                button.add-row(type="button" :disabled="mutation.value.except_source_cidrs.length >= 16" :title="mutation.value.except_source_cidrs.length >= 16 ? 'The server allows at most 16 source CIDR exceptions.' : undefined" @click="addXffException(mutationIndex)") + Add source CIDR
              article.route-card(v-for="(cidr, cidrIndex) in mutation.value.except_source_cidrs" :key="cidrIndex")
                header.route-card-heading
                  strong Source CIDR {{ cidrIndex + 1 }}
                  button.danger-link(type="button" :aria-label="`Remove source CIDR ${cidrIndex + 1}`" @click="removeXffException(mutationIndex, cidrIndex)") Remove
                label.field
                  span Canonical CIDR
                  input(type="text" :value="cidr" placeholder="127.0.0.0/8" @input="setXffException(mutationIndex, cidrIndex, $event)")

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
          label.field(v-if="mutation.operation !== 'remove'" data-field="http_services[].routes[].action.policy.response_headers[].value")
            span Header value
            input(type="text" v-model="mutation.value")
          label.enable-row(v-if="mutation.operation !== 'remove'" data-field="http_services[].routes[].action.policy.response_headers[].always")
            input(type="checkbox" v-model="mutation.always")
            span Add on every response status

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
          input(type="number" min="0" max="3" step="1" :value="action.policy.retry.max_retries" @input="setMaxRetries")
        label.field(data-field="http_services[].routes[].action.policy.retry.target")
          span Retry target
          select(:value="action.policy.retry.target" @change="setRetryTarget")
            option(value="next_server") Next available server
            option(value="same_server") Same server
        label.field(data-field="http_services[].routes[].action.policy.retry.delay_ms")
          span Delay (milliseconds)
          input(type="number" min="0" max="60000" step="1" v-model.number="action.policy.retry.delay_ms")
        label.field(data-field="http_services[].routes[].action.policy.retry.jitter_percent")
          span Delay jitter (percent)
          input(type="number" min="0" max="100" step="1" v-model.number="action.policy.retry.jitter_percent")
        label.enable-row(data-field="http_services[].routes[].action.policy.retry.method_safety")
          span(data-field="http_services[].routes[].action.policy.retry.body_safety")
            input(
              type="checkbox"
              :checked="allowsBufferedIdempotentRetries"
              @change="setBufferedIdempotentRetries"
            )
          span Allow replay of buffered idempotent requests, including POST
        small.field-hint
          | Enable only when every request handled by this route is idempotent. OxiRoute buffers the complete request body before replaying it.
        label.field(v-if="allowsBufferedIdempotentRetries" data-field="http_services[].routes[].action.policy.retry.idempotency_header")
          span Replay non-GET requests only with header
          input(type="text" :value="action.policy.retry.idempotency_header ?? ''" placeholder="Idempotency-Key" @input="setIdempotencyHeader")
        label.enable-row(data-field="http_services[].routes[].action.policy.retry.final_redispatch")
          input(
            type="checkbox"
            :disabled="action.policy.retry.target !== 'same_server' || action.policy.retry.max_retries <= 0"
            :title="action.policy.retry.target !== 'same_server' || action.policy.retry.max_retries <= 0 ? 'Final redispatch requires at least one same-server retry.' : undefined"
            v-model="action.policy.retry.final_redispatch"
          )
          span Redispatch the final retry to another server
      fieldset.object-block(data-field="http_services[].routes[].action.policy.retry.budget")
        legend Retry budget
        label.enable-row
          input(type="checkbox" :checked="action.policy.retry.budget !== null" @change="toggleRetryBudget")
          span Cap the retries of this route
        .field-grid(v-if="action.policy.retry.budget")
          label.field(data-field="http_services[].routes[].action.policy.retry.budget.per_second")
            span Retries per second
            input(type="number" min="1" max="100000" step="1" placeholder="unlimited" :value="action.policy.retry.budget.per_second ?? ''" @input="setBudgetLimit('per_second', $event)")
          label.field(data-field="http_services[].routes[].action.policy.retry.budget.ratio_percent")
            span Share of recent requests (percent)
            input(type="number" min="1" max="100" step="1" placeholder="unlimited" :value="action.policy.retry.budget.ratio_percent ?? ''" @input="setBudgetLimit('ratio_percent', $event)")
      fieldset.retry-triggers(data-field="http_services[].routes[].action.policy.retry.triggers")
        legend Retry triggers
        label.enable-row(v-for="trigger in HTTP_RETRY_TRIGGERS" :key="trigger")
          input(
            type="checkbox"
            :checked="action.policy.retry.triggers.includes(trigger)"
            :disabled="action.policy.retry.triggers.length === 1 && action.policy.retry.triggers.includes(trigger) && (action.policy.retry.response_statuses?.length ?? 0) === 0 && action.policy.retry.max_retries > 0"
            :title="action.policy.retry.triggers.length === 1 && action.policy.retry.triggers.includes(trigger) && (action.policy.retry.response_statuses?.length ?? 0) === 0 && action.policy.retry.max_retries > 0 ? 'The server requires a retry trigger or response status.' : undefined"
            @change="toggleRetryTrigger(trigger, $event)"
          )
          span {{ retryTriggerLabels[trigger] }}
      NumberListField(
        :model-value="action.policy.retry.response_statuses ?? []"
        label="Retry response statuses"
        item-label="status"
        field-path="http_services[].routes[].action.policy.retry.response_statuses"
        :default-value="500"
        :min="100"
        :max="599"
        :max-items="100"
        hint="Retry on explicitly selected upstream response statuses; these also count as passive health failures."
        @update:model-value="action.policy.retry.response_statuses = $event"
      )

    fieldset.object-block(data-field="http_services[].routes[].action.policy.timeouts")
      legend Upstream timeouts
      small.field-hint
        | Leave a phase empty to inherit the pool, route policy or service timeout. The total never exceeds the service upstream I/O timeout.
      .field-grid
        label.field(data-field="http_services[].routes[].action.policy.timeouts.connect_ms")
          span Connect timeout per attempt (ms)
          input(type="number" min="1" max="86400000" step="1" placeholder="inherit" :value="action.policy.timeouts.connect_ms ?? ''" @input="setTimeoutMs('connect_ms', $event)")
        label.field(data-field="http_services[].routes[].action.policy.timeouts.header_ms")
          span Response start timeout per attempt (ms)
          input(type="number" min="1" max="86400000" step="1" placeholder="inherit" :value="action.policy.timeouts.header_ms ?? ''" @input="setTimeoutMs('header_ms', $event)")
        label.field(data-field="http_services[].routes[].action.policy.timeouts.idle_ms")
          span Idle timeout between body chunks (ms)
          input(type="number" min="1" max="86400000" step="1" placeholder="inherit" :value="action.policy.timeouts.idle_ms ?? ''" @input="setTimeoutMs('idle_ms', $event)")
        label.field(data-field="http_services[].routes[].action.policy.timeouts.total_ms")
          span Total request deadline (ms)
          input(type="number" min="1" max="86400000" step="1" placeholder="inherit" :value="action.policy.timeouts.total_ms ?? ''" @input="setTimeoutMs('total_ms', $event)")

    label.enable-row(data-field="http_services[].routes[].action.policy.queue_headers")
      input(type="checkbox" v-model="action.policy.queue_headers")
      span Tell clients that waited for capacity their queue position and wait time
    small.field-hint
      | Adds x-oxiroute-queue-position and x-oxiroute-queue-wait-ms to those responses. Off by default because it reveals pool load.
    fieldset.object-block(data-field="http_services[].routes[].action.policy.rescue")
      legend Status rescue
      label.enable-row
        input(type="checkbox" :checked="action.policy.rescue !== null" @change="toggleRescue")
        span Replay a request on another server when the backend answers a listed status
      template(v-if="action.policy.rescue")
        small.field-hint
          | Spends the retry budget and follows the method and body safety rules. When no candidate is left, the client receives the last backend response unchanged.
        NumberListField(
          :model-value="action.policy.rescue.statuses"
          label="Rescued statuses"
          item-label="status"
          field-path="http_services[].routes[].action.policy.rescue.statuses"
          :default-value="404"
          :min="100"
          :max="599"
          :min-items="1"
          :max-items="100"
          @update:model-value="action.policy.rescue.statuses = $event"
        )
        .field-grid
          label.field(data-field="http_services[].routes[].action.policy.rescue.target")
            span Replay target
            select(:value="action.policy.rescue.target" @change="setRescueTarget")
              option(value="next_server") Another server of the pool
              option(value="pool") A rescue pool
          label.field(v-if="action.policy.rescue.target === 'pool'" data-field="http_services[].routes[].action.policy.rescue.pool")
            span Rescue pool
            select(v-model="action.policy.rescue.pool" required)
              option(value="") Select a pool
              option(v-for="name in poolNames.filter((pool) => pool !== action.upstream_pool)" :key="name" :value="name") {{ name }}

    HttpCachePolicyEditor(
      :model-value="action.policy.cache"
      field-path="http_services[].routes[].action.policy.cache"
      :store-names="cacheStoreNames"
      @update:model-value="action.policy.cache = $event"
    )
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type {
  HttpProxyActionConfig,
  HttpRequestHeaderValueConfig,
  HttpRoutePolicyConfig,
  HttpRetryTrigger,
} from '../config'
import { HTTP_RETRY_TRIGGERS, defaultUpstreamHost } from './httpDefaults'
import HttpCachePolicyEditor from './HttpCachePolicyEditor.vue'
import NumberListField from './NumberListField.vue'

const props = defineProps<{
  action: HttpProxyActionConfig
  routePolicy: HttpRoutePolicyConfig
  poolNames: string[]
  cacheStoreNames: string[]
}>()
const emit = defineEmits<{ changed: [] }>()

const allowsBufferedIdempotentRetries = computed(() =>
  props.action.policy.retry.method_safety === 'all' &&
  props.action.policy.retry.body_safety === 'buffered',
)

const retryTriggerLabels: Record<HttpRetryTrigger, string> = {
  connect_failure: 'Connection failure',
  connect_timeout: 'Connection timeout',
  refused_stream: 'HTTP/2 refused stream',
  empty_response: 'Empty response',
  response_timeout: 'Response timeout',
  junk_response: 'Malformed response',
}

function setTimeoutMs(key: keyof HttpProxyActionConfig['policy']['timeouts'], event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.action.policy.timeouts[key] = value === '' ? null : Number(value)
}

function setIdempotencyHeader(event: Event): void {
  props.action.policy.retry.idempotency_header = (event.target as HTMLInputElement).value || null
}

function toggleRetryBudget(event: Event): void {
  props.action.policy.retry.budget = (event.target as HTMLInputElement).checked
    ? { per_second: 10, ratio_percent: null }
    : null
}

function setBudgetLimit(key: 'per_second' | 'ratio_percent', event: Event): void {
  const budget = props.action.policy.retry.budget
  if (!budget) return
  const value = (event.target as HTMLInputElement).value
  budget[key] = value === '' ? null : Number(value)
}

function toggleRescue(event: Event): void {
  props.action.policy.rescue = (event.target as HTMLInputElement).checked
    ? { statuses: [404], target: 'next_server', pool: null }
    : null
}

function setRescueTarget(event: Event): void {
  const rescue = props.action.policy.rescue
  if (!rescue) return
  rescue.target = (event.target as HTMLSelectElement).value as 'next_server' | 'pool'
  rescue.pool = rescue.target === 'pool' ? '' : null
}

function setMaxRetries(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  if (value === '') return
  props.action.policy.retry.max_retries = Number(value)
  normalizeFinalRedispatch()
}

function setRetryTarget(event: Event): void {
  props.action.policy.retry.target = (event.target as HTMLSelectElement).value as
    HttpProxyActionConfig['policy']['retry']['target']
  normalizeFinalRedispatch()
}

function normalizeFinalRedispatch(): void {
  if (props.action.policy.retry.target !== 'same_server' || props.action.policy.retry.max_retries <= 0) {
    props.action.policy.retry.final_redispatch = false
  }
}

function setBufferedIdempotentRetries(event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (props.routePolicy.max_request_body_bytes === null) {
      props.routePolicy.max_request_body_bytes = 10_485_760
    }
    props.routePolicy.request_buffering = true
    props.action.policy.retry.method_safety = 'all'
    props.action.policy.retry.body_safety = 'buffered'
  } else {
    props.action.policy.retry.method_safety = 'get_head'
    props.action.policy.retry.body_safety = 'empty'
  }
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

function toggleUpstreamPathRewrite(event: Event): void {
  props.action.policy.upstream_path_rewrite = (event.target as HTMLInputElement).checked
    ? { from: '/', to: '/' }
    : null
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
  if (type === 'literal') current.value = { type, value: '' }
  else if (type === 'nginx_host') current.value = { type, fallback: 'localhost' }
  else if (type === 'appended_x_forwarded_for') {
    current.value = { type, max_bytes: 8_192, except_source_cidrs: [] }
  }
  else if (type === 'incoming_header') current.value = { type, name: '', max_bytes: 8_192 }
  else current.value = { type }
}

function addXffException(mutationIndex: number): void {
  const mutation = props.action.policy.request_headers[mutationIndex]
  if (mutation?.operation !== 'set' || mutation.value.type !== 'appended_x_forwarded_for' ||
    mutation.value.except_source_cidrs.length >= 16) return
  mutation.value.except_source_cidrs.push('')
  emit('changed')
}

function removeXffException(mutationIndex: number, cidrIndex: number): void {
  const mutation = props.action.policy.request_headers[mutationIndex]
  if (mutation?.operation !== 'set' || mutation.value.type !== 'appended_x_forwarded_for') return
  mutation.value.except_source_cidrs.splice(cidrIndex, 1)
  emit('changed')
}

function setXffException(mutationIndex: number, cidrIndex: number, event: Event): void {
  const mutation = props.action.policy.request_headers[mutationIndex]
  if (mutation?.operation !== 'set' || mutation.value.type !== 'appended_x_forwarded_for') return
  mutation.value.except_source_cidrs[cidrIndex] = (event.target as HTMLInputElement).value
}

function changeResponseOperation(index: number, event: Event): void {
  const current = props.action.policy.response_headers[index]
  if (!current) return
  const operation = (event.target as HTMLSelectElement).value
  props.action.policy.response_headers[index] = operation === 'remove'
    ? { operation, name: current.name }
    : { operation: operation as 'set' | 'add', name: current.name, value: '', always: true }
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
  } else if (triggers.length > 1 || (props.action.policy.retry.response_statuses?.length ?? 0) > 0 || props.action.policy.retry.max_retries === 0) {
    props.action.policy.retry.triggers = triggers.filter((candidate) => candidate !== trigger)
  }
}
</script>
