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

fieldset.object-block(data-field="forward_proxy_services[].peer_policy")
  legend Static HTTP/1 peers
  .field-grid
    label.field(data-field="forward_proxy_services[].peer_policy.direct_fallback")
      span Direct fallback
      select(v-model="service.peer_policy.direct_fallback")
        option(value="allowed") Allowed
        option(value="denied") Denied
        option(value="required") Required
    label.field(data-field="forward_proxy_services[].peer_policy.max_retries")
      span Peer retry budget
      input(type="number" min="0" max="15" step="1" v-model.number="service.peer_policy.max_retries")
  fieldset.route-list(data-field="forward_proxy_services[].peer_policy.peers")
    .route-heading
      legend Peer endpoints
      button.add-row(type="button" :disabled="service.peer_policy.peers.length >= 16" @click="addPeer") + Add peer
    p.empty-list(v-if="service.peer_policy.peers.length === 0") No static peers configured.
    article.route-card(v-for="(peer, peerIndex) in service.peer_policy.peers" :key="peerIndex")
      header.route-card-heading
        strong Peer {{ peerIndex + 1 }}
        button.danger-link(type="button" :aria-label="`Remove forward peer ${peerIndex + 1}`" @click="removePeer(peerIndex)") Remove
      .field-grid
        label.field(data-field="forward_proxy_services[].peer_policy.peers[].host")
          span Peer host
          input(type="text" v-model="peer.host" placeholder="proxy.example.test")
        label.field(data-field="forward_proxy_services[].peer_policy.peers[].port")
          span Peer port
          input(type="number" min="1" max="65535" step="1" v-model.number="peer.port")

fieldset.object-block(data-field="forward_proxy_services[].auth")
  legend Client authentication
  label.enable-row
    input(type="checkbox" :checked="service.auth !== null" @change="toggleAuth")
    span Require credentials loaded from a server file
  .field-grid(v-if="service.auth")
    label.field(data-field="forward_proxy_services[].auth.type")
      span Authentication type
      select(:value="service.auth.type" @change="changeAuthType")
        option(value="bearer_token_file") Bearer token file
        option(value="basic_htpasswd_file") Basic htpasswd file
        option(value="mutual_tls") Mutual TLS (reserved)
    label.field(v-if="service.auth.type === 'bearer_token_file'" data-field="forward_proxy_services[].auth.token_file_path")
      span Token file path
      input(type="text" v-model="service.auth.token_file_path" autocomplete="off" placeholder="/run/secrets/forward-proxy")
      small Authenticated configuration only; this path is suppressed from topology views.
    template(v-else-if="service.auth.type === 'basic_htpasswd_file'")
      label.field(data-field="forward_proxy_services[].auth.htpasswd_file_path")
        span htpasswd file path
        input(type="text" v-model="service.auth.htpasswd_file_path" autocomplete="off" placeholder="/etc/oxiroute/proxy.htpasswd")
      label.field(data-field="forward_proxy_services[].auth.realm")
        span Realm
        input(type="text" v-model="service.auth.realm")
      label.field(data-field="forward_proxy_services[].auth.credential_ttl_ms")
        span Validated credential TTL (ms)
        input(type="number" min="1" max="86400000" step="1" :value="service.auth.credential_ttl_ms ?? ''" @input="updateCredentialTtl")
      label.enable-row.compact-enable(data-field="forward_proxy_services[].auth.username_case_sensitive")
        input(type="checkbox" v-model="service.auth.username_case_sensitive")
        span Match usernames case-sensitively
    label.field(v-else data-field="forward_proxy_services[].auth.client_ca_file_path")
      span Client CA file path
      input(type="text" v-model="service.auth.client_ca_file_path" autocomplete="off" placeholder="/etc/oxiroute/client-ca.pem")

fieldset.object-block(data-field="forward_proxy_services[].access_policy")
  legend Ordered access policy
  label.enable-row
    input(type="checkbox" :checked="service.access_policy !== null" @change="toggleAccessPolicy")
    span Evaluate first-match access rules
  template(v-if="service.access_policy")
    label.field(data-field="forward_proxy_services[].access_policy.default_action")
      span Default action
      select(v-model="service.access_policy.default_action")
        option(value="deny") Deny
        option(value="allow") Allow
    .stack-list(data-field="forward_proxy_services[].access_policy.rules")
      article.object-block(v-for="(rule, ruleIndex) in service.access_policy.rules" :key="ruleIndex")
        .field-grid
          label.field(data-field="forward_proxy_services[].access_policy.rules[].action")
            span Rule action
            select(v-model="rule.action")
              option(value="deny") Deny
              option(value="allow") Allow
          button.danger-button(type="button" @click="removeAccessRule(ruleIndex)") Remove rule
        .stack-list(data-field="forward_proxy_services[].access_policy.rules[].conditions")
          article.object-block(v-for="(condition, conditionIndex) in rule.conditions" :key="conditionIndex")
            .field-grid
              label.field(data-field="forward_proxy_services[].access_policy.rules[].conditions[].type")
                span Matcher
                select(:value="condition.type" @change="changeConditionType(ruleIndex, conditionIndex, $event)")
                  option(value="all") All requests
                  option(value="methods") Methods
                  option(value="source_cidrs") Source CIDRs
                  option(value="destination_ports") Destination ports
                  option(value="authenticated") Authenticated
                  option(value="destination_local") Destination local
                  option(value="destination_link_local") Destination link-local
                  option(value="manager") Manager
              label.enable-row.compact-enable(data-field="forward_proxy_services[].access_policy.rules[].conditions[].negated")
                input(type="checkbox" v-model="condition.negated")
                span Negate matcher
              button.danger-button(type="button" @click="removeCondition(ruleIndex, conditionIndex)") Remove condition
            StringListField(v-if="condition.type === 'methods'" v-model="condition.methods" label="Methods" item-label="method" field-path="forward_proxy_services[].access_policy.rules[].conditions[].methods" :max-items="256")
            StringListField(v-if="condition.type === 'source_cidrs'" v-model="condition.cidrs" label="Source CIDRs" item-label="CIDR" field-path="forward_proxy_services[].access_policy.rules[].conditions[].cidrs" :max-items="256")
            .stack-list(v-if="condition.type === 'destination_ports'" data-field="forward_proxy_services[].access_policy.rules[].conditions[].ranges")
              .field-grid(v-for="(range, rangeIndex) in condition.ranges" :key="rangeIndex")
                label.field(data-field="forward_proxy_services[].access_policy.rules[].conditions[].ranges[].start")
                  span Start port
                  input(type="number" min="1" max="65535" step="1" v-model.number="range.start")
                label.field(data-field="forward_proxy_services[].access_policy.rules[].conditions[].ranges[].end")
                  span End port
                  input(type="number" :min="range.start" max="65535" step="1" v-model.number="range.end")
                button.danger-button(type="button" :disabled="condition.ranges.length === 1" @click="condition.ranges.splice(rangeIndex, 1)") Remove range
              button.secondary-button(type="button" @click="condition.ranges.push({ start: 443, end: 443 })") Add range
          button.secondary-button(type="button" @click="addCondition(ruleIndex)") Add condition
      button.secondary-button(type="button" @click="addAccessRule") Add rule

fieldset.object-block(data-field="forward_proxy_services[].header_policy")
  legend Forwarded metadata
  .field-grid
    label.field(data-field="forward_proxy_services[].header_policy.forwarded_for")
      span Forwarded and X-Forwarded-For
      select(v-model="service.header_policy.forwarded_for")
        option(value="delete") Delete
        option(value="preserve") Preserve
    label.field(data-field="forward_proxy_services[].header_policy.via")
      span Via
      select(v-model="service.header_policy.via")
        option(value="delete") Delete
        option(value="preserve") Preserve

fieldset.object-block(data-field="forward_proxy_services[].header_policy.cache")
  legend Forward response cache
  label.enable-row
    input(type="checkbox" :checked="service.cache != null" @change="toggleCache")
    span Enable bounded response caching for this forward service
  template(v-if="service.cache")
    .field-grid
      label.field(data-field="forward_proxy_services[].header_policy.cache.store")
        span Cache store
        input(type="text" v-model="service.cache.store" placeholder="memory-responses")
      label.field(data-field="forward_proxy_services[].header_policy.cache.default_ttl_ms")
        span Default TTL (ms)
        input(type="number" min="0" max="31536000000" step="1" v-model.number="service.cache.default_ttl_ms")
      label.field(data-field="forward_proxy_services[].header_policy.cache.grace_ms")
        span Grace period (ms)
        input(type="number" min="0" :max="Math.min(31536000000, service.cache.keep_ms)" step="1" v-model.number="service.cache.grace_ms")
      label.field(data-field="forward_proxy_services[].header_policy.cache.keep_ms")
        span Keep period (ms)
        input(type="number" :min="service.cache.grace_ms" max="31536000000" step="1" v-model.number="service.cache.keep_ms")
      label.field(data-field="forward_proxy_services[].header_policy.cache.set_cookie_policy")
        span Set-Cookie policy
        select(v-model="service.cache.set_cookie_policy")
          option(value="bypass") Bypass cache
          option(value="ignore") Ignore Set-Cookie
      label.field(data-field="forward_proxy_services[].header_policy.cache.authorization_policy")
        span Authorization policy
        select(v-model="service.cache.authorization_policy")
          option(value="bypass") Bypass cache
          option(value="cache") Allow caching
      label.field(data-field="forward_proxy_services[].header_policy.cache.vary_policy")
        span Vary policy
        select(v-model="service.cache.vary_policy")
          option(value="respect") Respect Vary
          option(value="ignore") Ignore Vary
    .field-grid
      label.enable-row.compact-enable(data-field="forward_proxy_services[].header_policy.cache.use_origin_cache_control")
        input(type="checkbox" v-model="service.cache.use_origin_cache_control")
        span Use origin Cache-Control
      label.enable-row.compact-enable(data-field="forward_proxy_services[].header_policy.cache.revalidate")
        input(type="checkbox" v-model="service.cache.revalidate")
        span Revalidate stale entries
      label.enable-row.compact-enable(data-field="forward_proxy_services[].header_policy.cache.collapsed_forwarding")
        input(type="checkbox" v-model="service.cache.collapsed_forwarding")
        span Collapse concurrent fills
    fieldset.retry-triggers(data-field="forward_proxy_services[].header_policy.cache.methods")
      legend Cacheable methods
      label.enable-row(v-for="method in ['GET', 'HEAD']" :key="method")
        input(type="checkbox" :checked="service.cache.methods.includes(method)" :disabled="service.cache.methods.length === 1 && service.cache.methods.includes(method)" @change="toggleCacheValue(service.cache.methods, method, $event)")
        span {{ method }}
    fieldset.route-list(data-field="forward_proxy_services[].header_policy.cache.key_components")
      .route-heading
        legend Cache key components
        button.add-row(type="button" :disabled="service.cache.key_components.length >= 32" @click="service.cache.key_components.push({ type: 'scheme' })") + Add component
      article.route-card(v-for="(component, index) in service.cache.key_components" :key="index")
        header.route-card-heading
          strong Key component {{ index + 1 }}
          button.danger-link(type="button" :disabled="service.cache.key_components.length === 1" @click="service.cache.key_components.splice(index, 1)") Remove
        .field-grid
          label.field(data-field="forward_proxy_services[].header_policy.cache.key_components[].type")
            span Component type
            select(:value="component.type" @change="changeCacheKeyComponent(index, $event)")
              option(value="scheme") Scheme
              option(value="normalized_host") Normalized host
              option(value="path_and_query") Path and query
              option(value="header") Request header
              option(value="cookie") Request cookie
          label.field(v-if="component.type === 'header' || component.type === 'cookie'" data-field="forward_proxy_services[].header_policy.cache.key_components[].name")
            span {{ component.type === 'header' ? 'Header' : 'Cookie' }} name
            input(type="text" v-model="component.name")
    fieldset.route-list(data-field="forward_proxy_services[].header_policy.cache.status_ttls")
      .route-heading
        legend Status TTL overrides
        button.add-row(type="button" :disabled="service.cache.status_ttls.length >= 64" @click="service.cache.status_ttls.push({ status: 200, ttl_ms: 60000 })") + Add status TTL
      article.route-card(v-for="(entry, index) in service.cache.status_ttls" :key="index")
        header.route-card-heading
          strong Status TTL {{ index + 1 }}
          button.danger-link(type="button" @click="service.cache.status_ttls.splice(index, 1)") Remove
        .field-grid
          label.field(data-field="forward_proxy_services[].header_policy.cache.status_ttls[].status")
            span HTTP status
            input(type="number" min="100" max="599" step="1" v-model.number="entry.status")
          label.field(data-field="forward_proxy_services[].header_policy.cache.status_ttls[].ttl_ms")
            span TTL (ms)
            input(type="number" min="0" max="31536000000" step="1" v-model.number="entry.ttl_ms")
    fieldset.retry-triggers(data-field="forward_proxy_services[].header_policy.cache.stale_on")
      legend Serve stale on
      label.enable-row(v-for="trigger in CACHE_STALE_TRIGGERS" :key="trigger")
        input(type="checkbox" :checked="service.cache.stale_on.includes(trigger)" @change="toggleCacheValue(service.cache.stale_on, trigger, $event)")
        span {{ trigger.replaceAll('_', ' ') }}
    fieldset.route-list(data-field="forward_proxy_services[].header_policy.cache.bypass_request")
      .route-heading
        legend Bypass request predicates
        button.add-row(type="button" :disabled="service.cache.bypass_request.length >= 32" @click="addCachePredicate(service.cache.bypass_request)") + Add predicate
      article.route-card(v-for="(predicate, index) in service.cache.bypass_request" :key="index")
        .field-grid
          label.field(data-field="forward_proxy_services[].header_policy.cache.bypass_request[].type")
            span Predicate type
            select(v-model="predicate.type")
              option(value="header_present") Header present
              option(value="cookie_present") Cookie present
          label.field(data-field="forward_proxy_services[].header_policy.cache.bypass_request[].name")
            span Predicate name
            input(type="text" v-model="predicate.name")
          button.danger-button(type="button" @click="service.cache.bypass_request.splice(index, 1)") Remove predicate
    fieldset.route-list(data-field="forward_proxy_services[].header_policy.cache.no_store_request")
      .route-heading
        legend No-store request predicates
        button.add-row(type="button" :disabled="service.cache.no_store_request.length >= 32" @click="addCachePredicate(service.cache.no_store_request)") + Add predicate
      article.route-card(v-for="(predicate, index) in service.cache.no_store_request" :key="index")
        .field-grid
          label.field(data-field="forward_proxy_services[].header_policy.cache.no_store_request[].type")
            span Predicate type
            select(v-model="predicate.type")
              option(value="header_present") Header present
              option(value="cookie_present") Cookie present
          label.field(data-field="forward_proxy_services[].header_policy.cache.no_store_request[].name")
            span Predicate name
            input(type="text" v-model="predicate.name")
          button.danger-button(type="button" @click="service.cache.no_store_request.splice(index, 1)") Remove predicate
    fieldset.route-list(data-field="forward_proxy_services[].header_policy.cache.no_store_response")
      .route-heading
        legend No-store response predicates
        button.add-row(type="button" :disabled="service.cache.no_store_response.length >= 32" @click="addCachePredicate(service.cache.no_store_response)") + Add predicate
      article.route-card(v-for="(predicate, index) in service.cache.no_store_response" :key="index")
        .field-grid
          label.field(data-field="forward_proxy_services[].header_policy.cache.no_store_response[].type")
            span Predicate type
            select(v-model="predicate.type")
              option(value="header_present") Header present
              option(value="cookie_present") Cookie present
          label.field(data-field="forward_proxy_services[].header_policy.cache.no_store_response[].name")
            span Predicate name
            input(type="text" v-model="predicate.name")
          button.danger-button(type="button" @click="service.cache.no_store_response.splice(index, 1)") Remove predicate
    fieldset.object-block(data-field="forward_proxy_services[].header_policy.cache.surrogate_tags")
      legend Surrogate tags
      label.enable-row
        input(type="checkbox" :checked="service.cache.surrogate_tags !== null" @change="toggleCacheSurrogateTags")
        span Read bounded surrogate tags from an origin response header
      .field-grid(v-if="service.cache.surrogate_tags")
        label.field(data-field="forward_proxy_services[].header_policy.cache.surrogate_tags.response_header")
          span Response header
          input(type="text" v-model="service.cache.surrogate_tags.response_header")
        label.field(data-field="forward_proxy_services[].header_policy.cache.surrogate_tags.max_tags")
          span Maximum tags
          input(type="number" min="1" max="256" step="1" v-model.number="service.cache.surrogate_tags.max_tags")
        label.field(data-field="forward_proxy_services[].header_policy.cache.surrogate_tags.max_tag_bytes")
          span Maximum tag bytes
          input(type="number" min="1" max="1024" step="1" v-model.number="service.cache.surrogate_tags.max_tag_bytes")
    fieldset.object-block(data-field="forward_proxy_services[].header_policy.cache.purge_authorization")
      legend Purge authorization
      label.enable-row
        input(type="checkbox" :checked="service.cache.purge_authorization !== null" @change="toggleCachePurgeAuthorization")
        span Require a bearer token loaded from a server file
      .field-grid(v-if="service.cache.purge_authorization")
        label.field(data-field="forward_proxy_services[].header_policy.cache.purge_authorization.type")
          span Authorization type
          select(v-model="service.cache.purge_authorization.type" disabled)
            option(value="bearer_token_file") Bearer token file
        label.field(data-field="forward_proxy_services[].header_policy.cache.purge_authorization.token_file_path")
          span Token file path
          input(type="text" v-model="service.cache.purge_authorization.token_file_path" autocomplete="off")

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
  .time-range-list(data-field="forward_proxy_services[].destination_policy.allow_times")
    .route-heading
      legend Allow windows
      button.secondary-button(type="button" @click="addTimeRange('allow_times')") Add window
    article.object-block(v-for="(range, rangeIndex) in service.destination_policy.allow_times" :key="`allow-${rangeIndex}`")
      .field-grid
        label.field(data-field="forward_proxy_services[].destination_policy.allow_times[].days")
          span Days
          select(multiple v-model="range.days")
            option(v-for="day in FORWARD_WEEKDAYS" :key="day" :value="day") {{ day }}
        label.field(data-field="forward_proxy_services[].destination_policy.allow_times[].start")
          span Start (UTC)
          input(type="time" v-model="range.start")
        label.field(data-field="forward_proxy_services[].destination_policy.allow_times[].end")
          span End (UTC)
          input(type="time" v-model="range.end")
        button.danger-button(type="button" @click="removeTimeRange('allow_times', rangeIndex)") Remove window
  .time-range-list(data-field="forward_proxy_services[].destination_policy.deny_times")
    .route-heading
      legend Deny windows
      button.secondary-button(type="button" @click="addTimeRange('deny_times')") Add window
    article.object-block(v-for="(range, rangeIndex) in service.destination_policy.deny_times" :key="`deny-${rangeIndex}`")
      .field-grid
        label.field(data-field="forward_proxy_services[].destination_policy.deny_times[].days")
          span Days
          select(multiple v-model="range.days")
            option(v-for="day in FORWARD_WEEKDAYS" :key="day" :value="day") {{ day }}
        label.field(data-field="forward_proxy_services[].destination_policy.deny_times[].start")
          span Start (UTC)
          input(type="time" v-model="range.start")
        label.field(data-field="forward_proxy_services[].destination_policy.deny_times[].end")
          span End (UTC)
          input(type="time" v-model="range.end")
        button.danger-button(type="button" @click="removeTimeRange('deny_times', rangeIndex)") Remove window

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
      input(type="number" min="8192" max="1048576" step="1" v-model.number="service.max_header_bytes")
    label.field(data-field="forward_proxy_services[].max_connections")
      span Maximum connections
      input(type="number" min="1" max="1000000" step="1" v-model.number="service.max_connections")

fieldset.object-block(data-field="forward_proxy_services[].resolver")
  legend Bounded DNS resolver
  StringListField(v-model="service.resolver.nameservers" label="Nameservers" item-label="IP address" field-path="forward_proxy_services[].resolver.nameservers" :max-items="8")
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
      span Revalidate on connect
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type {
  CacheKeyComponentConfig,
  CachePredicateConfig,
  ForwardAccessConditionConfig,
  ForwardHttpVersion,
  ForwardProxyServiceConfig,
} from '../config'
import {
  CACHE_STALE_TRIGGERS,
  FORWARD_HTTP_VERSIONS,
  FORWARD_WEEKDAYS,
  defaultHttpCachePolicy,
} from './canonicalDefaults'
import NumberListField from './NumberListField.vue'

const props = defineProps<{ service: ForwardProxyServiceConfig }>()
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

function toggleCache(event: Event): void {
  props.service.cache = (event.target as HTMLInputElement).checked
    ? defaultHttpCachePolicy()
    : null
}

function toggleCacheValue(values: string[], value: string, event: Event): void {
  if ((event.target as HTMLInputElement).checked) {
    if (!values.includes(value)) values.push(value)
  } else {
    const index = values.indexOf(value)
    if (index >= 0) values.splice(index, 1)
  }
}

function changeCacheKeyComponent(index: number, event: Event): void {
  if (!props.service.cache) return
  const type = (event.target as HTMLSelectElement).value as CacheKeyComponentConfig['type']
  props.service.cache.key_components[index] = type === 'header' || type === 'cookie'
    ? { type, name: '' }
    : { type }
}

function addCachePredicate(predicates: CachePredicateConfig[]): void {
  if (predicates.length < 32) predicates.push({ type: 'header_present', name: '' })
}

function toggleCacheSurrogateTags(event: Event): void {
  if (!props.service.cache) return
  props.service.cache.surrogate_tags = (event.target as HTMLInputElement).checked
    ? { response_header: 'surrogate-key', max_tags: 64, max_tag_bytes: 256 }
    : null
}

function toggleCachePurgeAuthorization(event: Event): void {
  if (!props.service.cache) return
  props.service.cache.purge_authorization = (event.target as HTMLInputElement).checked
    ? { type: 'bearer_token_file', token_file_path: '' }
    : null
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
