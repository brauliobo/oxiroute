<template lang="pug">
fieldset.object-block(:data-field="basePath")
  legend {{ operation === 'publish' ? 'Publish' : 'Play' }} access policy
  fieldset.route-list(:data-field="`${basePath}.rules`")
    .route-heading
      legend ACL rules
      button.add-row(type="button" :disabled="policy.rules.length >= 64" @click="addRule") + Add rule
    p.empty-list(v-if="policy.rules.length === 0") No network rule is configured.
    article.route-card(v-for="(rule, ruleIndex) in policy.rules" :key="ruleIndex")
      header.route-card-heading
        strong Rule {{ ruleIndex + 1 }}
        button.danger-link(type="button" :aria-label="`Remove ${operation} rule ${ruleIndex + 1}`" @click="removeRule(ruleIndex)") Remove
      .field-grid
        template(v-if="operation === 'publish'")
          label.field(data-field="rtmp_services[].applications[].publish.rules[].action")
            span Action
            select(v-model="rule.action")
              option(value="allow") Allow
              option(value="deny") Deny
          label.field(data-field="rtmp_services[].applications[].publish.rules[].network")
            span Network
            input(type="text" v-model="rule.network" placeholder="all or 192.0.2.0/24")
        template(v-else)
          label.field(data-field="rtmp_services[].applications[].play.rules[].action")
            span Action
            select(v-model="rule.action")
              option(value="allow") Allow
              option(value="deny") Deny
          label.field(data-field="rtmp_services[].applications[].play.rules[].network")
            span Network
            input(type="text" v-model="rule.network" placeholder="all or 192.0.2.0/24")
  .field-grid
    label.field(:data-field="`${basePath}.token`")
      span Stream-query token
      select(:value="policy.token?.source ?? 'disabled'" @change="setToken")
        option(value="disabled") Disabled
        option(value="stream_query") Stream query
    template(v-if="policy.token")
      template(v-if="operation === 'publish'")
        label.field(data-field="rtmp_services[].applications[].publish.token.source")
          span Token source
          select(v-model="policy.token.source")
            option(value="stream_query") Stream query
        label.field(data-field="rtmp_services[].applications[].publish.token.parameter")
          span Query parameter
          input(type="text" v-model="policy.token.parameter" maxlength="32" autocomplete="off")
        label.field(data-field="rtmp_services[].applications[].publish.token.secret")
          span Token secret
          input(type="password" v-model="policy.token.secret" maxlength="128" autocomplete="new-password")
      template(v-else)
        label.field(data-field="rtmp_services[].applications[].play.token.source")
          span Token source
          select(v-model="policy.token.source")
            option(value="stream_query") Stream query
        label.field(data-field="rtmp_services[].applications[].play.token.parameter")
          span Query parameter
          input(type="text" v-model="policy.token.parameter" maxlength="32" autocomplete="off")
        label.field(data-field="rtmp_services[].applications[].play.token.secret")
          span Token secret
          input(type="password" v-model="policy.token.secret" maxlength="128" autocomplete="new-password")
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type { RtmpAccessPolicyConfig } from '../config'

const props = defineProps<{
  policy: RtmpAccessPolicyConfig
  operation: 'publish' | 'play'
}>()

const basePath = computed(() => `rtmp_services[].applications[].${props.operation}`)

function addRule(): void {
  if (props.policy.rules.length >= 64) return
  props.policy.rules.push({ action: 'allow', network: 'all' })
}

function removeRule(index: number): void {
  props.policy.rules.splice(index, 1)
}

function setToken(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  props.policy.token = value === 'stream_query'
    ? props.policy.token ?? { source: 'stream_query', parameter: 'token', secret: '' }
    : null
}
</script>
