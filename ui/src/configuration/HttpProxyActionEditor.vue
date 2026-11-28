<template lang="pug">
.action-editor
  ConfigField(path="http_services[].routes[].action.upstream_pool" label="Upstream pool")
    select.ox-input(v-model="action.upstream_pool" required)
      option(value="") Select a pool
      option(v-for="name in poolNames" :key="name" :value="name") {{ name }}

  ConfigGroup.object-block(path="http_services[].routes[].action.policy" legend="Proxy policy")
    ConfigGroup.object-block(path="http_services[].routes[].action.policy.upstream_host" legend="Upstream Host header")
      .field-grid
        ConfigField(path="http_services[].routes[].action.policy.upstream_host.type" label="Host policy")
          Select(
            :model-value="action.policy.upstream_host.type" @change="changeUpstreamHost"
            :options="[{ value: 'preserve_incoming', label: 'Preserve incoming authority' }, { value: 'nginx_host', label: 'nginx $host with fallback' }, { value: 'endpoint', label: 'Selected endpoint authority' }, { value: 'literal', label: 'Literal authority' }]"
          )
        ConfigField(v-if="action.policy.upstream_host.type === 'endpoint'" path="http_services[].routes[].action.policy.upstream_host.unix_fallback" label="Unix endpoint fallback")
          Input(:model-value="action.policy.upstream_host.unix_fallback ?? ''" placeholder="localhost" @input="setUnixFallback")
          small Required when the selected pool contains a Unix endpoint.
        ConfigField(v-else-if="action.policy.upstream_host.type === 'nginx_host'" path="http_services[].routes[].action.policy.upstream_host.fallback" label="Missing-Host fallback")
          Input(v-model="action.policy.upstream_host.fallback" placeholder="localhost")
        ConfigField(v-else-if="action.policy.upstream_host.type === 'literal'" path="http_services[].routes[].action.policy.upstream_host.value" label="Literal authority")
          Input(v-model="action.policy.upstream_host.value" placeholder="origin.example.test")

    ConfigGroup.object-block(path="http_services[].routes[].action.policy.upstream_path_rewrite" legend="Upstream path rewrite")
      Checkbox.enable-row(:model-value="action.policy.upstream_path_rewrite !== null" @change="toggleUpstreamPathRewrite") Rewrite the matched path before proxying
      .field-grid(v-if="action.policy.upstream_path_rewrite")
        ConfigField(path="http_services[].routes[].action.policy.upstream_path_rewrite.from" label="Source prefix")
          Input(v-model="action.policy.upstream_path_rewrite.from" placeholder="/public")
        ConfigField(path="http_services[].routes[].action.policy.upstream_path_rewrite.to" label="Destination prefix")
          Input(v-model="action.policy.upstream_path_rewrite.to" placeholder="/internal")

    ConfigGroup.route-list(path="http_services[].routes[].action.policy.request_headers")
      template(#legend)
        .cfg-route-heading
          legend Request header mutations
          Button.add-row(:disabled="action.policy.request_headers.length >= 32" :title="action.policy.request_headers.length >= 32 ? 'The server allows at most 32 request header mutations.' : undefined" @click="addHeader(action.policy.request_headers)") + Add request mutation
      ItemScope.route-card(v-for="(mutation, mutationIndex) in action.policy.request_headers" :key="mutationIndex" :index="mutationIndex")
        header.cfg-route-card-heading
          strong Request mutation {{ mutationIndex + 1 }}
          Button.danger-link(variant="danger" small :aria-label="`Remove request header mutation ${mutationIndex + 1}`" @click="removeHeader(action.policy.request_headers, mutationIndex)") Remove
        .field-grid
          ConfigField(path="http_services[].routes[].action.policy.request_headers[].operation" label="Operation")
            Select(
              :model-value="mutation.operation" @change="changeRequestOperation(mutationIndex, $event)"
              :options="[{ value: 'set', label: 'Set' }, { value: 'remove', label: 'Remove' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.request_headers[].name" label="Header name")
            Input(v-model="mutation.name" placeholder="x-forwarded-for")
          ConfigGroup.object-block(v-if="mutation.operation === 'set'" path="http_services[].routes[].action.policy.request_headers[].value" legend="Value source")
            .field-grid
              ConfigField(path="http_services[].routes[].action.policy.request_headers[].value.type" label="Source type")
                Select(
                  :model-value="mutation.value.type" @change="changeRequestValue(mutationIndex, $event)"
                  :options="[{ value: 'literal', label: 'Literal' }, { value: 'incoming_authority', label: 'Incoming authority' }, { value: 'normalized_host', label: 'Normalized host' }, { value: 'nginx_host', label: 'nginx $host with fallback' }, { value: 'client_ip', label: 'Client IP' }, { value: 'appended_x_forwarded_for', label: 'Append client IP to X-Forwarded-For' }, { value: 'downstream_scheme', label: 'Downstream scheme' }, { value: 'incoming_header', label: 'Incoming request header' }, { value: 'selected_upstream_host', label: 'Selected upstream Host' }]"
                )
              ConfigField(v-if="mutation.value.type === 'literal'" path="http_services[].routes[].action.policy.request_headers[].value.value" label="Literal value")
                Input(v-model="mutation.value.value")
              ConfigField(v-if="mutation.value.type === 'nginx_host'" path="http_services[].routes[].action.policy.request_headers[].value.fallback" label="Missing-Host fallback")
                Input(v-model="mutation.value.fallback" placeholder="localhost")
              ConfigField(v-if="mutation.value.type === 'incoming_header'" path="http_services[].routes[].action.policy.request_headers[].value.name" label="Source header name")
                Input(v-model="mutation.value.name" placeholder="x-request-id")
              ConfigField(v-if="mutation.value.type === 'incoming_header'" path="http_services[].routes[].action.policy.request_headers[].value.max_bytes" label="Maximum header bytes")
                NumberInput(min="1" step="1" v-model="mutation.value.max_bytes")
              ConfigField(v-if="mutation.value.type === 'appended_x_forwarded_for'" path="http_services[].routes[].action.policy.request_headers[].value.max_bytes" label="Maximum header bytes")
                NumberInput(min="1" step="1" v-model="mutation.value.max_bytes")
            ConfigGroup.route-list(v-if="mutation.value.type === 'appended_x_forwarded_for'" path="http_services[].routes[].action.policy.request_headers[].value.except_source_cidrs")
              template(#legend)
                .cfg-route-heading
                  legend Source CIDR exceptions
                  Button.add-row(:disabled="mutation.value.except_source_cidrs.length >= 16" :title="mutation.value.except_source_cidrs.length >= 16 ? 'The server allows at most 16 source CIDR exceptions.' : undefined" @click="addXffException(mutationIndex)") + Add source CIDR
              ItemScope.route-card(v-for="(cidr, cidrIndex) in mutation.value.except_source_cidrs" :key="cidrIndex" :index="cidrIndex")
                header.cfg-route-card-heading
                  strong Source CIDR {{ cidrIndex + 1 }}
                  Button.danger-link(variant="danger" small :aria-label="`Remove source CIDR ${cidrIndex + 1}`" @click="removeXffException(mutationIndex, cidrIndex)") Remove
                label.field
                  span Canonical CIDR
                  Input(:model-value="cidr" placeholder="127.0.0.0/8" @input="setXffException(mutationIndex, cidrIndex, $event)")

    ConfigGroup.route-list(path="http_services[].routes[].action.policy.response_headers")
      template(#legend)
        .cfg-route-heading
          legend Response header mutations
          Button.add-row(:disabled="action.policy.response_headers.length >= 32" :title="action.policy.response_headers.length >= 32 ? 'The server allows at most 32 response header mutations.' : undefined" @click="addHeader(action.policy.response_headers)") + Add response mutation
      ItemScope.route-card(v-for="(mutation, mutationIndex) in action.policy.response_headers" :key="mutationIndex" :index="mutationIndex")
        header.cfg-route-card-heading
          strong Response mutation {{ mutationIndex + 1 }}
          Button.danger-link(variant="danger" small :aria-label="`Remove response header mutation ${mutationIndex + 1}`" @click="removeHeader(action.policy.response_headers, mutationIndex)") Remove
        .field-grid
          ConfigField(path="http_services[].routes[].action.policy.response_headers[].operation" label="Operation")
            Select(
              :model-value="mutation.operation" @change="changeResponseOperation(mutationIndex, $event)"
              :options="[{ value: 'set', label: 'Set' }, { value: 'add', label: 'Add' }, { value: 'remove', label: 'Remove' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.response_headers[].name" label="Header name")
            Input(v-model="mutation.name")
          ConfigField(v-if="mutation.operation !== 'remove'" path="http_services[].routes[].action.policy.response_headers[].value" label="Header value")
            Input(v-model="mutation.value")
          ConfigField(v-if="mutation.operation !== 'remove'" path="http_services[].routes[].action.policy.response_headers[].always" label="Add on every response status" check v-model="mutation.always")

    ConfigGroup.route-list(path="http_services[].routes[].action.policy.response_cookie_path_rewrites")
      template(#legend)
        .cfg-route-heading
          legend Response cookie path rewrites
          Button.add-row(:disabled="action.policy.response_cookie_path_rewrites.length >= 16" :title="action.policy.response_cookie_path_rewrites.length >= 16 ? 'The server allows at most 16 cookie path rewrites.' : undefined" @click="addCookieRewrite") + Add cookie rewrite
      ItemScope.route-card(v-for="(rewrite, rewriteIndex) in action.policy.response_cookie_path_rewrites" :key="rewriteIndex" :index="rewriteIndex")
        header.cfg-route-card-heading
          strong Cookie rewrite {{ rewriteIndex + 1 }}
          Button.danger-link(variant="danger" small :aria-label="`Remove cookie path rewrite ${rewriteIndex + 1}`" @click="removeCookieRewrite(rewriteIndex)") Remove
        .field-grid
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_path_rewrites[].from" label="Source path")
            Input(v-model="rewrite.from" placeholder="/")
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_path_rewrites[].to" label="Replacement path")
            Input(v-model="rewrite.to" placeholder="/app")
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_path_rewrites[].mode" label="Match")
            Select(
              :model-value="rewrite.mode ?? 'exact'" @change="setCookieRewriteMode(rewrite, $event)"
              :options="[{ value: 'exact', label: 'Exact path' }, { value: 'nginx_prefix', label: 'nginx prefix (verbatim replacement)' }]"
            )

    ConfigGroup.route-list(path="http_services[].routes[].action.policy.response_cookie_attributes")
      template(#legend)
        .cfg-route-heading
          legend Response cookie attributes
          Button.add-row(:disabled="action.policy.response_cookie_attributes.length >= 16" @click="addCookieAttribute") + Add cookie policy
      ItemScope.route-card(v-for="(cookieAttr, attrIndex) in action.policy.response_cookie_attributes" :key="attrIndex" :index="attrIndex")
        header.cfg-route-card-heading
          strong Cookie policy {{ attrIndex + 1 }}
          Button.danger-link(variant="danger" small @click="removeCookieAttribute(attrIndex)") Remove
        .field-grid
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_attributes[].name" label="Cookie name")
            Input(v-model="cookieAttr.name" placeholder="session_id")
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_attributes[].secure" label="Secure flag")
            Select(
              :model-value="triState(cookieAttr.secure)" @update:model-value="cookieAttr.secure = parseTriState($event)"
              :options="[{ value: 'inherit', label: 'Unchanged' }, { value: 'true', label: 'Force true' }, { value: 'false', label: 'Force false' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_attributes[].http_only" label="HttpOnly flag")
            Select(
              :model-value="triState(cookieAttr.http_only)" @update:model-value="cookieAttr.http_only = parseTriState($event)"
              :options="[{ value: 'inherit', label: 'Unchanged' }, { value: 'true', label: 'Force true' }, { value: 'false', label: 'Force false' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.response_cookie_attributes[].same_site" label="SameSite")
            Select(
              :model-value="cookieAttr.same_site ?? 'inherit'" @update:model-value="setCookieSameSite(cookieAttr, $event)"
              :options="[{ value: 'inherit', label: 'Unchanged' }, { value: 'strict', label: 'Strict' }, { value: 'lax', label: 'Lax' }, { value: 'none', label: 'None' }]"
            )

    ConfigGroup.object-block(path="http_services[].routes[].action.policy.retry" legend="Retry policy")
      .field-grid
        ConfigField(path="http_services[].routes[].action.policy.retry.max_retries" label="Maximum retries")
          NumberInput(min="0" max="3" step="1" :model-value="action.policy.retry.max_retries" @input="setMaxRetries")
        ConfigField(path="http_services[].routes[].action.policy.retry.target" label="Retry target")
          Select(
            :model-value="action.policy.retry.target" @change="setRetryTarget"
            :options="[{ value: 'next_server', label: 'Next available server' }, { value: 'same_server', label: 'Same server' }]"
          )
        ConfigField(path="http_services[].routes[].action.policy.retry.delay_ms" label="Delay (milliseconds)")
          NumberInput(min="0" max="60000" step="1" v-model="action.policy.retry.delay_ms")
        ConfigField(path="http_services[].routes[].action.policy.retry.jitter_percent" label="Delay jitter (percent)")
          NumberInput(min="0" max="100" step="1" v-model="action.policy.retry.jitter_percent")
        ConfigField(path="http_services[].routes[].action.policy.retry.method_safety" label="Allow replay of buffered idempotent requests, including POST" check)
          span(data-field="http_services[].routes[].action.policy.retry.body_safety")
            input(
              type="checkbox"
              :checked="allowsBufferedIdempotentRetries"
              @change="setBufferedIdempotentRetries"
            )
        small.field-hint
          | Enable only when every request handled by this route is idempotent. OxiRoute buffers the complete request body before replaying it.
        ConfigField(v-if="allowsBufferedIdempotentRetries" path="http_services[].routes[].action.policy.retry.idempotency_header" label="Replay non-GET requests only with header")
          Input(:model-value="action.policy.retry.idempotency_header ?? ''" placeholder="Idempotency-Key" @input="setIdempotencyHeader")
        ConfigField(path="http_services[].routes[].action.policy.retry.final_redispatch" label="Redispatch the final retry to another server" check)
          input(
            type="checkbox"
            :disabled="action.policy.retry.target !== 'same_server' || action.policy.retry.max_retries <= 0"
            :title="action.policy.retry.target !== 'same_server' || action.policy.retry.max_retries <= 0 ? 'Final redispatch requires at least one same-server retry.' : undefined"
            v-model="action.policy.retry.final_redispatch"
          )
      ConfigGroup.object-block(path="http_services[].routes[].action.policy.retry.budget" legend="Retry budget")
        Checkbox.enable-row(:model-value="action.policy.retry.budget !== null" @change="toggleRetryBudget") Cap the retries of this route
        .field-grid(v-if="action.policy.retry.budget")
          ConfigField(path="http_services[].routes[].action.policy.retry.budget.per_second" label="Retries per second")
            NumberInput(min="1" max="100000" step="1" placeholder="unlimited" :model-value="action.policy.retry.budget.per_second ?? ''" @input="setBudgetLimit('per_second', $event)")
          ConfigField(path="http_services[].routes[].action.policy.retry.budget.ratio_percent" label="Share of recent requests (percent)")
            NumberInput(min="1" max="100" step="1" placeholder="unlimited" :model-value="action.policy.retry.budget.ratio_percent ?? ''" @input="setBudgetLimit('ratio_percent', $event)")
      ConfigGroup.retry-triggers(path="http_services[].routes[].action.policy.retry.triggers" legend="Retry triggers")
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

    ConfigGroup.object-block(path="http_services[].routes[].action.policy.timeouts" legend="Upstream timeouts")
      small.field-hint
        | Leave a phase empty to inherit the pool, route policy or service timeout. The total never exceeds the service upstream I/O timeout.
      .field-grid
        ConfigField(path="http_services[].routes[].action.policy.timeouts.connect_ms" label="Connect timeout per attempt (ms)")
          NumberInput(min="1" max="86400000" step="1" placeholder="inherit" :model-value="action.policy.timeouts.connect_ms ?? ''" @input="setTimeoutMs('connect_ms', $event)")
        ConfigField(path="http_services[].routes[].action.policy.timeouts.header_ms" label="Response start timeout per attempt (ms)")
          NumberInput(min="1" max="86400000" step="1" placeholder="inherit" :model-value="action.policy.timeouts.header_ms ?? ''" @input="setTimeoutMs('header_ms', $event)")
        ConfigField(path="http_services[].routes[].action.policy.timeouts.idle_ms" label="Idle timeout between body chunks (ms)")
          NumberInput(min="1" max="86400000" step="1" placeholder="inherit" :model-value="action.policy.timeouts.idle_ms ?? ''" @input="setTimeoutMs('idle_ms', $event)")
        ConfigField(path="http_services[].routes[].action.policy.timeouts.total_ms" label="Total request deadline (ms)")
          NumberInput(min="1" max="86400000" step="1" placeholder="inherit" :model-value="action.policy.timeouts.total_ms ?? ''" @input="setTimeoutMs('total_ms', $event)")

    .field-grid
      ConfigField(path="http_services[].routes[].action.policy.queue_class" label="Queue class override")
        Input(:model-value="action.policy.queue_class ?? ''" placeholder="Inherit pool default or header" @input="setQueueClass")
    ConfigField(path="http_services[].routes[].action.policy.queue_headers" label="Tell clients that waited for capacity their queue position and wait time" check v-model="action.policy.queue_headers")
    small.field-hint
      | Adds x-oxiroute-queue-position and x-oxiroute-queue-wait-ms to those responses. Off by default because it reveals pool load.

    ConfigGroup.object-block(path="http_services[].routes[].action.policy.affinity" legend="Session affinity")
      Checkbox.enable-row(:model-value="action.policy.affinity != null" @change="toggleAffinity") Stick client requests to a preferred backend server
      .field-grid(v-if="action.policy.affinity")
        ConfigField(path="http_services[].routes[].action.policy.affinity.type" label="Affinity mode")
          Select(
            :model-value="action.policy.affinity.type" @update:model-value="changeAffinityType($event)"
            :options="[{ value: 'cookie', label: 'Managed cookie' }, { value: 'hash', label: 'Consistent hash' }, { value: 'header', label: 'Request header' }]"
          )
        template(v-if="action.policy.affinity.type === 'cookie'")
          ConfigField(path="http_services[].routes[].action.policy.affinity.name" label="Cookie name")
            Input(v-model="action.policy.affinity.name" placeholder="ox_srv")
          ConfigField(path="http_services[].routes[].action.policy.affinity.ttl_s" label="Cookie TTL (s)")
            NumberInput(min="1" step="1" v-model="action.policy.affinity.ttl_s")
          ConfigField(path="http_services[].routes[].action.policy.affinity.same_site" label="SameSite")
            Select(
              v-model="action.policy.affinity.same_site"
              :options="[{ value: 'lax', label: 'Lax' }, { value: 'strict', label: 'Strict' }, { value: 'none', label: 'None' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.affinity.secure" label="Secure cookie" check v-model="action.policy.affinity.secure")
          ConfigField(path="http_services[].routes[].action.policy.affinity.http_only" label="HttpOnly cookie" check v-model="action.policy.affinity.http_only")
        ConfigField(v-else-if="action.policy.affinity.type === 'hash'" path="http_services[].routes[].action.policy.affinity.key" label="Hash key expression")
          Input(v-model="action.policy.affinity.key" placeholder="client_ip")
        ConfigField(v-else path="http_services[].routes[].action.policy.affinity.name" label="Header name")
          Input(v-model="action.policy.affinity.name" placeholder="x-tenant-id")

    ConfigGroup.object-block(path="http_services[].routes[].action.policy.select" legend="Header-based pool selection")
      Checkbox.enable-row(:model-value="action.policy.select != null" @change="togglePoolSelect") Route requests to alternate pools based on a numeric header
      template(v-if="action.policy.select")
        .field-grid
          ConfigField(path="http_services[].routes[].action.policy.select.header" label="Selector header")
            Input(v-model="action.policy.select.header" placeholder="x-priority")
          ConfigField(path="http_services[].routes[].action.policy.select.on_invalid" label="On missing/invalid header")
            Select(
              v-model="action.policy.select.on_invalid"
              :options="[{ value: 'default', label: 'Use default pool' }, { value: 'reject', label: 'Reject request' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.select.spill" label="Capacity spill")
            Select(
              v-model="action.policy.select.spill"
              :options="[{ value: 'none', label: 'None' }, { value: 'up', label: 'Spill up to next pool' }]"
            )
          ConfigField(path="http_services[].routes[].action.policy.select.forward" label="Forward selector header upstream" check v-model="action.policy.select.forward")
        ConfigGroup.route-list(path="http_services[].routes[].action.policy.select.rules")
          template(#legend)
            .cfg-route-heading
              legend Selection rules
              Button.add-row(@click="addSelectRule") + Add rule
          ItemScope.route-card(v-for="(rule, ruleIndex) in action.policy.select.rules" :key="ruleIndex" :index="ruleIndex")
            header.cfg-route-card-heading
              strong Rule {{ ruleIndex + 1 }}
              Button.danger-link(variant="danger" small @click="removeSelectRule(ruleIndex)") Remove
            .field-grid
              ConfigField(path="http_services[].routes[].action.policy.select.rules[].at_most" label="At most")
                NumberInput(min="0" step="1" v-model="rule.at_most")
              ConfigField(path="http_services[].routes[].action.policy.select.rules[].pool" label="Target pool")
                Select(
                  v-model="rule.pool"
                  :options="poolNames.map((name) => ({ value: name, label: name }))"
                )
    ConfigGroup.object-block(path="http_services[].routes[].action.policy.rescue" legend="Status rescue")
      Checkbox.enable-row(:model-value="action.policy.rescue !== null" @change="toggleRescue") Replay a request on another server when the backend answers a listed status
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
          ConfigField(path="http_services[].routes[].action.policy.rescue.target" label="Replay target")
            Select(
              :model-value="action.policy.rescue.target" @change="setRescueTarget"
              :options="[{ value: 'next_server', label: 'Another server of the pool' }, { value: 'pool', label: 'A rescue pool' }]"
            )
          ConfigField(v-if="action.policy.rescue.target === 'pool'" path="http_services[].routes[].action.policy.rescue.pool" label="Rescue pool")
            select.ox-input(v-model="action.policy.rescue.pool" required)
              option(value="") Select a pool
              option(v-for="name in poolNames.filter((pool) => pool !== action.upstream_pool)" :key="name" :value="name") {{ name }}

    HttpCachePolicyEditor(
      :model-value="action.policy.cache"
      field-path="http_services[].routes[].action.policy.cache"
      :store-names="cacheStoreNames"
      @update:model-value="action.policy.cache = $event"
    )
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import { computed } from 'vue'

import type {
  HttpCookiePathRewriteConfig,
  HttpProxyActionConfig,
  HttpRequestHeaderMutationConfig,
  HttpRequestHeaderValueConfig,
  HttpResponseHeaderMutationConfig,
  HttpRoutePolicyConfig,
  HttpRetryTrigger,
} from '../config'
import { HTTP_RETRY_TRIGGERS, defaultUpstreamHost } from './httpDefaults'
import HttpCachePolicyEditor from './HttpCachePolicyEditor.vue'
import NumberListField from './NumberListField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

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

type HeaderMutations = HttpRequestHeaderMutationConfig[] | HttpResponseHeaderMutationConfig[]

function addHeader(headers: HeaderMutations): void {
  if (headers.length >= 32) return
  headers.push({ operation: 'remove', name: '' })
  emit('changed')
}

function removeHeader(headers: HeaderMutations, index: number): void {
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

function setCookieRewriteMode(rewrite: HttpCookiePathRewriteConfig, event: Event): void {
  rewrite.mode = (event.target as HTMLSelectElement).value === 'nginx_prefix' ? 'nginx_prefix' : 'exact'
}

function removeCookieRewrite(index: number): void {
  props.action.policy.response_cookie_path_rewrites.splice(index, 1)
  emit('changed')
}

function addCookieAttribute(): void {
  if (props.action.policy.response_cookie_attributes.length >= 16) return
  props.action.policy.response_cookie_attributes.push({ name: '', secure: null, http_only: null, same_site: null })
  emit('changed')
}

function removeCookieAttribute(index: number): void {
  props.action.policy.response_cookie_attributes.splice(index, 1)
  emit('changed')
}

function triState(value: boolean | null): string {
  return value === null ? 'inherit' : value ? 'true' : 'false'
}

function parseTriState(value: string): boolean | null {
  return value === 'inherit' ? null : value === 'true'
}

function setCookieSameSite(
  cookieAttr: HttpProxyActionConfig['policy']['response_cookie_attributes'][number],
  value: string,
): void {
  cookieAttr.same_site = value === 'strict' || value === 'lax' || value === 'none' ? value : null
}

function setQueueClass(event: Event): void {
  props.action.policy.queue_class = (event.target as HTMLInputElement).value || null
}

function toggleAffinity(event: Event): void {
  props.action.policy.affinity = (event.target as HTMLInputElement).checked
    ? { type: 'cookie', name: 'ox_srv', ttl_s: 3600, same_site: 'lax', secure: true, http_only: true }
    : null
}

function changeAffinityType(type: string): void {
  if (type === 'hash') props.action.policy.affinity = { type: 'hash', key: 'client_ip' }
  else if (type === 'header') props.action.policy.affinity = { type: 'header', name: 'x-tenant-id' }
  else props.action.policy.affinity = { type: 'cookie', name: 'ox_srv', ttl_s: 3600, same_site: 'lax', secure: true, http_only: true }
}

function togglePoolSelect(event: Event): void {
  props.action.policy.select = (event.target as HTMLInputElement).checked
    ? {
        header: 'x-priority',
        rules: [{ at_most: 10, pool: props.poolNames[0] ?? props.action.upstream_pool }],
        on_invalid: 'default',
        forward: false,
        spill: 'none',
      }
    : null
}

function addSelectRule(): void {
  props.action.policy.select?.rules.push({ at_most: 100, pool: props.poolNames[0] ?? props.action.upstream_pool })
  emit('changed')
}

function removeSelectRule(index: number): void {
  props.action.policy.select?.rules.splice(index, 1)
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
