<template lang="pug">
header.form-heading
  div
    p.eyebrow RTMP dispatch
    h3 {{ service.name || 'Unnamed RTMP service' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove service
.field-grid
  label.field(data-field="rtmp_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
  label.field(data-field="rtmp_services[].outbound_chunk_size")
    span Outbound chunk size (bytes)
    input(type="number" min="1" max="1048576" step="1" v-model.number="service.outbound_chunk_size")
  label.field(data-field="rtmp_services[].access_log.type")
    span Session access log
    select(:value="service.access_log?.type ?? 'default'" @change="setAccessLog")
      option(value="default") Runtime default
      option(value="disabled") Disabled
fieldset.object-block(data-field="rtmp_services[].outbound_policy")
  legend Outbound relay policy
  .field-grid
    StringListField(v-model="service.outbound_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="rtmp_services[].outbound_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_domains" label="Denied domains" item-label="domain" field-path="rtmp_services[].outbound_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.deny_cidrs" :max-items="256")
    label.enable-row.compact-enable(data-field="rtmp_services[].outbound_policy.deny_private")
      input(type="checkbox" v-model="service.outbound_policy.deny_private")
      span Deny private destinations
    label.field(data-field="rtmp_services[].outbound_policy.rtmps")
      span RTMPS policy
      select(v-model="service.outbound_policy.rtmps")
        option(value="disabled") Disabled
        option(value="allowed") Allowed
        option(value="required") Required
    label.field(data-field="rtmp_services[].outbound_policy.max_chain_depth")
      span Maximum relay chain depth
      input(type="number" min="1" max="16" step="1" v-model.number="service.outbound_policy.max_chain_depth")
fieldset.object-block(data-field="rtmp_services[].callbacks")
  legend Service callbacks
  .field-grid
    label.field(data-field="rtmp_services[].callbacks.on_connect")
      span Connect callback
      input(type="text" v-model="service.callbacks.on_connect" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_disconnect")
      span Disconnect callback
      input(type="text" v-model="service.callbacks.on_disconnect" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_publish")
      span Publish callback
      input(type="text" v-model="service.callbacks.on_publish" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_publish_done")
      span Publish done callback
      input(type="text" v-model="service.callbacks.on_publish_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_play")
      span Play callback
      input(type="text" v-model="service.callbacks.on_play" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_play_done")
      span Play done callback
      input(type="text" v-model="service.callbacks.on_play_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_done")
      span Done callback
      input(type="text" v-model="service.callbacks.on_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_update")
      span Update callback
      input(type="text" v-model="service.callbacks.on_update" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.notify_method")
      span Callback method
      select(v-model="service.callbacks.notify_method")
        option(value="post") POST
        option(value="get") GET
    label.field(data-field="rtmp_services[].callbacks.timeout_ms")
      span Callback timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.callbacks.timeout_ms")
    label.field(data-field="rtmp_services[].callbacks.notify_update_timeout_ms")
      span Update timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.callbacks.notify_update_timeout_ms")
    label.enable-row.compact-enable(data-field="rtmp_services[].callbacks.notify_update_strict")
      input(type="checkbox" v-model="service.callbacks.notify_update_strict")
      span Require update callback success
    label.enable-row.compact-enable(data-field="rtmp_services[].callbacks.notify_relay_redirect")
      input(type="checkbox" v-model="service.callbacks.notify_relay_redirect")
      span Notify relay redirects
fieldset.route-list(data-field="rtmp_services[].exec_profiles")
  .route-heading
    legend Exec profiles
    button.add-row(
      type="button"
      :disabled="(service.exec_profiles?.length ?? 0) >= 64"
      @click="addExecProfile"
    ) + Add exec profile
  p.empty-list(v-if="!service.exec_profiles || service.exec_profiles.length === 0") No exec profile is configured for this service.
  article.route-card(v-for="(profile, profileIndex) in service.exec_profiles || []" :key="profileIndex")
    header.route-card-heading
      strong Exec profile {{ profileIndex + 1 }}
      button.danger-link(type="button" @click="removeExecProfile(profileIndex)") Remove
    .field-grid
      label.field(data-field="rtmp_services[].exec_profiles[].name")
        span Profile name
        input(type="text" v-model="profile.name")
      label.field(data-field="rtmp_services[].exec_profiles[].application")
        span Application
        select(v-model="profile.application")
          option(v-for="application in service.applications" :key="application.name" :value="application.name") {{ application.name || 'Unnamed application' }}
      label.field(data-field="rtmp_services[].exec_profiles[].mode")
        span Exec mode
        select(v-model="profile.mode")
          option(value="command") Command
          option(value="transcode") Transcode
      label.field(data-field="rtmp_services[].exec_profiles[].trigger")
        span Trigger
        select(v-model="profile.trigger")
          option(value="publisher") Publisher
          option(value="publish_done") Publish done
      label.field(data-field="rtmp_services[].exec_profiles[].executable")
        span Executable
        input(type="text" v-model="profile.executable" placeholder="/usr/bin/ffmpeg")
      StringListField(
        v-model="profile.arguments"
        label="Arguments"
        item-label="argument"
        field-path="rtmp_services[].exec_profiles[].arguments"
        :max-items="64"
      )
      label.field(data-field="rtmp_services[].exec_profiles[].working_directory")
        span Working directory
        input(type="text" v-model="profile.working_directory" placeholder="/var/lib/oxiroute/exec")
      label.field(data-field="rtmp_services[].exec_profiles[].filesystem")
        span Filesystem policy
        select(v-model="profile.filesystem")
          option(value="working_directory") Working directory only
          option(value="host") Host (rejected by runtime)
      label.field(data-field="rtmp_services[].exec_profiles[].network")
        span Network policy
        select(v-model="profile.network")
          option(value="disabled") Disabled
          option(value="inherited") Inherited
      label.field(data-field="rtmp_services[].exec_profiles[].timeout_ms")
        span Timeout (ms)
        input(type="number" min="1" max="86400000" step="1" v-model.number="profile.timeout_ms")
      label.field(data-field="rtmp_services[].exec_profiles[].shutdown_timeout_ms")
        span Shutdown timeout (ms)
        input(type="number" min="1" max="60000" step="1" v-model.number="profile.shutdown_timeout_ms")
      label.field(data-field="rtmp_services[].exec_profiles[].max_processes")
        span Maximum processes
        input(type="number" min="1" max="256" step="1" v-model.number="profile.max_processes")
      label.field(data-field="rtmp_services[].exec_profiles[].max_queue_messages")
        span Maximum queue messages
        input(type="number" min="1" max="65536" step="1" v-model.number="profile.max_queue_messages")
      label.field(data-field="rtmp_services[].exec_profiles[].max_queue_bytes")
        span Maximum queue bytes
        input(type="number" min="1" max="1073741824" step="1" v-model.number="profile.max_queue_bytes")
      label.field(data-field="rtmp_services[].exec_profiles[].max_stdout_bytes")
        span Maximum stdout bytes
        input(type="number" min="1" max="16777216" step="1" v-model.number="profile.max_stdout_bytes")
      label.field(data-field="rtmp_services[].exec_profiles[].max_stderr_bytes")
        span Maximum stderr bytes
        input(type="number" min="1" max="16777216" step="1" v-model.number="profile.max_stderr_bytes")
      label.enable-row.compact-enable(data-field="rtmp_services[].exec_profiles[].respawn")
        input(type="checkbox" v-model="profile.respawn")
        span Respawn on exit
      label.field(data-field="rtmp_services[].exec_profiles[].respawn_delay_ms")
        span Respawn delay (ms)
        input(type="number" min="1" max="300000" step="1" v-model.number="profile.respawn_delay_ms")
      label.field(data-field="rtmp_services[].exec_profiles[].max_respawns")
        span Maximum respawns
        input(type="number" min="0" max="64" step="1" v-model.number="profile.max_respawns")
    fieldset.object-block(data-field="rtmp_services[].exec_profiles[].environment")
      legend Environment
      button.secondary-button(type="button" :disabled="profile.environment.length >= 32" @click="addExecEnvironment(profile)") + Add variable
      p.empty-list(v-if="profile.environment.length === 0") No environment variables are configured.
      .field-grid(v-for="(environment, environmentIndex) in profile.environment" :key="environmentIndex")
        label.field(data-field="rtmp_services[].exec_profiles[].environment[].name")
          span Name
          input(type="text" v-model="environment.name")
        label.field(data-field="rtmp_services[].exec_profiles[].environment[].value")
          span Value
          input(type="text" v-model="environment.value" autocomplete="off")
        button.danger-link(type="button" @click="profile.environment.splice(environmentIndex, 1)") Remove
fieldset.route-list(data-field="rtmp_services[].applications")
  .route-heading
    legend Applications
    button.add-row(
      type="button"
      :disabled="service.applications.length >= 256"
      :title="service.applications.length >= 256 ? 'The server allows at most 256 applications per RTMP service.' : undefined"
      @click="addApplication"
    ) + Add application
  p.empty-list(v-if="service.applications.length === 0") At least one application is required.
  article.route-card(v-for="(application, applicationIndex) in service.applications" :key="applicationIndex")
    header.route-card-heading
      strong Application {{ applicationIndex + 1 }}
      button.danger-link(type="button" :aria-label="`Remove RTMP application ${applicationIndex + 1}`" @click="removeApplication(applicationIndex)") Remove
    .field-grid
      label.field(data-field="rtmp_services[].applications[].name")
        span Application name
        input(type="text" v-model="application.name")
      label.enable-row.compact-enable(data-field="rtmp_services[].applications[].live")
        input(
          type="checkbox"
          v-model="application.live"
          :disabled="application.recorders.length > 0"
          :title="application.recorders.length > 0 ? 'Remove configured recorders before disabling live publishing.' : undefined"
        )
        span Allow live publishing
      label.enable-row.compact-enable(data-field="rtmp_services[].applications[].idle_streams")
        input(type="checkbox" v-model="application.idle_streams")
        span Allow viewers before a publisher
      RtmpAccessPolicyEditor(:policy="application.publish" operation="publish")
      RtmpAccessPolicyEditor(:policy="application.play" operation="play")
      fieldset.object-block(data-field="rtmp_services[].applications[].limits")
        legend Session ceilings
        .field-grid
          label.field(data-field="rtmp_services[].applications[].limits.max_connections")
            span Maximum connections
            input(type="number" min="1" max="100000" step="1" v-model.number="application.limits.max_connections")
          label.field(data-field="rtmp_services[].applications[].limits.max_publishers")
            span Maximum publishers
            input(type="number" min="1" max="10000" step="1" v-model.number="application.limits.max_publishers")
          label.field(data-field="rtmp_services[].applications[].limits.max_viewers")
            span Maximum viewers
            input(type="number" min="1" max="1000000" step="1" v-model.number="application.limits.max_viewers")
      fieldset.object-block(data-field="rtmp_services[].applications[].hls")
        legend HLS output
        label.enable-row
          input(type="checkbox" :checked="application.hls != null" @change="toggleHls(application, $event)")
          span Publish HLS media output
        template(v-if="application.hls")
          .field-grid
            label.field(data-field="rtmp_services[].applications[].hls.root_directory")
              span HLS root directory
              input(type="text" v-model="application.hls.root_directory" placeholder="/var/lib/oxiroute/hls")
            label.field(data-field="rtmp_services[].applications[].hls.segment_duration_ms")
              span Segment duration (ms)
              input(type="number" min="100" max="60000" step="1" v-model.number="application.hls.segment_duration_ms")
            label.field(data-field="rtmp_services[].applications[].hls.max_segment_duration_ms")
              span Maximum segment duration (ms)
              input(type="number" min="100" max="120000" step="1" v-model.number="application.hls.max_segment_duration_ms")
            label.field(data-field="rtmp_services[].applications[].hls.playlist_length_ms")
              span Playlist length (ms)
              input(type="number" min="100" max="86400000" step="1" v-model.number="application.hls.playlist_length_ms")
            label.field(data-field="rtmp_services[].applications[].hls.fragment_naming")
              span Fragment naming
              select(v-model="application.hls.fragment_naming")
                option(value="sequential") Sequential
                option(value="timestamp") Timestamp
                option(value="system") System
            label.field(data-field="rtmp_services[].applications[].hls.max_segment_bytes")
              span Maximum segment bytes
              input(type="number" min="1" max="1073741824" step="1" v-model.number="application.hls.max_segment_bytes")
            label.field(data-field="rtmp_services[].applications[].hls.max_queue_messages")
              span Maximum queue messages
              input(type="number" min="1" max="65536" step="1" v-model.number="application.hls.max_queue_messages")
            label.field(data-field="rtmp_services[].applications[].hls.max_storage_bytes")
              span Maximum storage bytes
              input(type="number" min="1" max="1099511627776" step="1" v-model.number="application.hls.max_storage_bytes")
            label.field(data-field="rtmp_services[].applications[].hls.max_storage_files")
              span Maximum storage files
              input(type="number" min="1" max="1000000" step="1" v-model.number="application.hls.max_storage_files")
            label.field(data-field="rtmp_services[].applications[].hls.max_active_streams")
              span Maximum active streams
              input(type="number" min="1" max="100000" step="1" v-model.number="application.hls.max_active_streams")
          .field-grid
            label.enable-row.compact-enable(data-field="rtmp_services[].applications[].hls.nested")
              input(type="checkbox" v-model="application.hls.nested")
              span Nest media under the stream name
            label.enable-row.compact-enable(data-field="rtmp_services[].applications[].hls.cleanup")
              input(type="checkbox" v-model="application.hls.cleanup")
              span Remove expired media files
          fieldset.object-block(data-field="rtmp_services[].applications[].hls.keys")
            legend AES-128 keys
            label.enable-row
              input(type="checkbox" :checked="application.hls.keys != null" @change="toggleHlsKeys(application, $event)")
              span Rotate encrypted HLS keys
            .field-grid(v-if="application.hls.keys")
              label.field(data-field="rtmp_services[].applications[].hls.keys.rotation_segments")
                span Segments per key
                input(type="number" min="1" max="10000" step="1" v-model.number="application.hls.keys.rotation_segments")
              label.field(data-field="rtmp_services[].applications[].hls.keys.url_prefix")
                span Key URL prefix
                input(type="text" v-model="application.hls.keys.url_prefix" placeholder="/media/keys/")
          fieldset.route-list(data-field="rtmp_services[].applications[].hls.variants")
            .route-heading
              legend HLS variants
              button.add-row(type="button" :disabled="application.hls.variants.length >= 16" @click="application.hls.variants.push({ name: '', bandwidth: 1000000, codecs: null, width: null, height: null })") + Add variant
            article.route-card(v-for="(variant, variantIndex) in application.hls.variants" :key="variantIndex")
              header.route-card-heading
                strong Variant {{ variantIndex + 1 }}
                button.danger-link(type="button" @click="application.hls.variants.splice(variantIndex, 1)") Remove
              .field-grid
                label.field(data-field="rtmp_services[].applications[].hls.variants[].name")
                  span Name
                  input(type="text" v-model="variant.name")
                label.field(data-field="rtmp_services[].applications[].hls.variants[].bandwidth")
                  span Bandwidth (bps)
                  input(type="number" min="1" max="1000000000" step="1" v-model.number="variant.bandwidth")
                label.field(data-field="rtmp_services[].applications[].hls.variants[].codecs")
                  span Codecs
                  input(type="text" :value="variant.codecs ?? ''" @input="setNullableVariantField(variant, 'codecs', $event)")
                label.field(data-field="rtmp_services[].applications[].hls.variants[].width")
                  span Width
                  input(type="number" min="1" max="10000" step="1" :value="variant.width ?? ''" @input="setNullableVariantField(variant, 'width', $event)")
                label.field(data-field="rtmp_services[].applications[].hls.variants[].height")
                  span Height
                  input(type="number" min="1" max="10000" step="1" :value="variant.height ?? ''" @input="setNullableVariantField(variant, 'height', $event)")
      fieldset.object-block(data-field="rtmp_services[].applications[].dash")
        legend MPEG-DASH output
        label.enable-row
          input(type="checkbox" :checked="application.dash != null" @change="toggleDash(application, $event)")
          span Configure DASH output (validation reports unsupported runtime capability)
        .field-grid(v-if="application.dash")
          label.field(data-field="rtmp_services[].applications[].dash.root_directory")
            span DASH root directory
            input(type="text" v-model="application.dash.root_directory" placeholder="/var/lib/oxiroute/dash")
          label.field(data-field="rtmp_services[].applications[].dash.segment_duration_ms")
            span Segment duration (ms)
            input(type="number" min="100" max="60000" step="1" v-model.number="application.dash.segment_duration_ms")
          label.field(data-field="rtmp_services[].applications[].dash.max_segment_duration_ms")
            span Maximum segment duration (ms)
            input(type="number" min="100" max="120000" step="1" v-model.number="application.dash.max_segment_duration_ms")
          label.field(data-field="rtmp_services[].applications[].dash.playlist_length_ms")
            span Playlist length (ms)
            input(type="number" min="100" max="86400000" step="1" v-model.number="application.dash.playlist_length_ms")
          label.field(data-field="rtmp_services[].applications[].dash.segment_naming")
            span Segment naming
            select(v-model="application.dash.segment_naming")
              option(value="sequential") Sequential
              option(value="timestamp") Timestamp
              option(value="system") System
          label.field(data-field="rtmp_services[].applications[].dash.max_segment_bytes")
            span Maximum segment bytes
            input(type="number" min="1" max="67108864" step="1" v-model.number="application.dash.max_segment_bytes")
          label.field(data-field="rtmp_services[].applications[].dash.max_queue_messages")
            span Maximum queue messages
            input(type="number" min="1" max="65536" step="1" v-model.number="application.dash.max_queue_messages")
          label.field(data-field="rtmp_services[].applications[].dash.max_storage_bytes")
            span Maximum storage bytes
            input(type="number" min="1" max="1099511627776" step="1" v-model.number="application.dash.max_storage_bytes")
          label.field(data-field="rtmp_services[].applications[].dash.max_storage_files")
            span Maximum storage files
            input(type="number" min="1" max="1000000" step="1" v-model.number="application.dash.max_storage_files")
          label.field(data-field="rtmp_services[].applications[].dash.max_active_streams")
            span Maximum active streams
            input(type="number" min="1" max="100000" step="1" v-model.number="application.dash.max_active_streams")
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].dash.nested")
            input(type="checkbox" v-model="application.dash.nested")
            span Nest media under the stream name
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].dash.cleanup")
            input(type="checkbox" v-model="application.dash.cleanup")
            span Remove expired media files
      fieldset.object-block(data-field="rtmp_services[].applications[].relay")
        legend Relay bounds
        .field-grid
          label.field(data-field="rtmp_services[].applications[].relay.max_queue_messages")
            span Maximum relay messages
            input(type="number" min="1" max="65536" step="1" v-model.number="application.relay.max_queue_messages")
          label.field(data-field="rtmp_services[].applications[].relay.max_queue_bytes")
            span Maximum relay bytes
            input(type="number" min="1" max="1073741824" step="1" v-model.number="application.relay.max_queue_bytes")
          label.field(data-field="rtmp_services[].applications[].relay.buffer_ms")
            span Relay buffer (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.buffer_ms")
          label.field(data-field="rtmp_services[].applications[].relay.push_reconnect_ms")
            span Push reconnect (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.push_reconnect_ms")
          label.field(data-field="rtmp_services[].applications[].relay.pull_reconnect_ms")
            span Pull reconnect (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.pull_reconnect_ms")
          label.field(data-field="rtmp_services[].applications[].relay.connect_timeout_ms")
            span Relay connect timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.connect_timeout_ms")
          label.field(data-field="rtmp_services[].applications[].relay.handshake_timeout_ms")
            span Relay handshake timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.handshake_timeout_ms")
      fieldset.object-block(data-field="rtmp_services[].applications[].callbacks")
        legend Application callbacks
        .field-grid
          label.field(data-field="rtmp_services[].applications[].callbacks.on_connect")
            span Connect callback
            input(type="text" v-model="application.callbacks.on_connect" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_disconnect")
            span Disconnect callback
            input(type="text" v-model="application.callbacks.on_disconnect" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_publish")
            span Publish callback
            input(type="text" v-model="application.callbacks.on_publish" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_publish_done")
            span Publish done callback
            input(type="text" v-model="application.callbacks.on_publish_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_play")
            span Play callback
            input(type="text" v-model="application.callbacks.on_play" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_play_done")
            span Play done callback
            input(type="text" v-model="application.callbacks.on_play_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_done")
            span Done callback
            input(type="text" v-model="application.callbacks.on_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_update")
            span Update callback
            input(type="text" v-model="application.callbacks.on_update" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.notify_method")
            span Callback method
            select(v-model="application.callbacks.notify_method")
              option(value="post") POST
              option(value="get") GET
          label.field(data-field="rtmp_services[].applications[].callbacks.timeout_ms")
            span Callback timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.callbacks.timeout_ms")
          label.field(data-field="rtmp_services[].applications[].callbacks.notify_update_timeout_ms")
            span Update timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.callbacks.notify_update_timeout_ms")
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].callbacks.notify_update_strict")
            input(type="checkbox" v-model="application.callbacks.notify_update_strict")
            span Require update callback success
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].callbacks.notify_relay_redirect")
            input(type="checkbox" v-model="application.callbacks.notify_relay_redirect")
            span Notify relay redirects
    fieldset.object-block(data-field="rtmp_services[].applications[].fanout")
      legend Fanout bounds
      .field-grid
        label.field(data-field="rtmp_services[].applications[].fanout.max_subscribers")
          span Maximum subscribers
          input(type="number" min="1" max="1000000" step="1" v-model.number="application.fanout.max_subscribers")
        label.field(data-field="rtmp_services[].applications[].fanout.max_queue_messages_per_subscriber")
          span Queue messages per subscriber
          input(type="number" min="1" max="65536" step="1" v-model.number="application.fanout.max_queue_messages_per_subscriber")
        label.field(data-field="rtmp_services[].applications[].fanout.max_queue_bytes_per_subscriber")
          span Queue bytes per subscriber
          input(type="number" min="1" max="1073741824" step="1" v-model.number="application.fanout.max_queue_bytes_per_subscriber")
    fieldset.route-list(data-field="rtmp_services[].applications[].pull_targets")
      .route-heading
        legend Pull relays
        button.add-row(type="button" :disabled="application.pull_targets.length >= 16" @click="addPullTarget(applicationIndex)") + Add pull target
      p.empty-list(v-if="application.pull_targets.length === 0") No inbound relay is configured.
      article.route-card(v-for="(target, targetIndex) in application.pull_targets" :key="targetIndex")
        header.route-card-heading
          strong Pull target {{ targetIndex + 1 }}
          button.danger-link(type="button" @click="removePullTarget(applicationIndex, targetIndex)") Remove
        .field-grid
          label.field(data-field="rtmp_services[].applications[].pull_targets[].host")
            span Host
            input(type="text" v-model="target.host")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].port")
            span Port
            input(type="number" min="1" max="65535" step="1" v-model.number="target.port")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].application")
            span Source application
            input(type="text" v-model="target.application")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].stream_name")
            span Source stream
            input(type="text" v-model="target.stream_name")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].scheme")
            span Transport
            select(v-model="target.scheme")
              option(value="rtmp") RTMP
              option(value="rtmps") RTMPS
          label.field(data-field="rtmp_services[].applications[].pull_targets[].tc_url")
            span TC URL
            input(type="text" :value="target.tc_url ?? ''" @input="setNullableTargetField(target, 'tc_url', $event)")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].flash_version")
            span Flash version
            input(type="text" :value="target.flash_version ?? ''" @input="setNullableTargetField(target, 'flash_version', $event)")
          label.enable-row(data-field="rtmp_services[].applications[].pull_targets[].credentials")
            input(type="checkbox" :checked="target.credentials !== null" @change="toggleTargetCredentials(target)")
            span Use credentials
          template(v-if="target.credentials")
            label.field(data-field="rtmp_services[].applications[].pull_targets[].credentials.username")
              span Username
              input(type="text" v-model="target.credentials.username")
            label.field(data-field="rtmp_services[].applications[].pull_targets[].credentials.secret_file")
              span Secret file
              input(type="text" v-model="target.credentials.secret_file" autocomplete="off")
    fieldset.route-list(data-field="rtmp_services[].applications[].push_targets")
      .route-heading
        legend Push relays
        button.add-row(type="button" :disabled="!application.live || application.push_targets.length >= 16" @click="addPushTarget(applicationIndex)") + Add push target
      p.empty-list(v-if="application.push_targets.length === 0") No outbound relay is configured.
      article.route-card(v-for="(target, targetIndex) in application.push_targets" :key="targetIndex")
        header.route-card-heading
          strong Push target {{ targetIndex + 1 }}
          button.danger-link(type="button" @click="removePushTarget(applicationIndex, targetIndex)") Remove
        .field-grid
          label.field(data-field="rtmp_services[].applications[].push_targets[].host")
            span Host
            input(type="text" v-model="target.host")
          label.field(data-field="rtmp_services[].applications[].push_targets[].port")
            span Port
            input(type="number" min="1" max="65535" step="1" v-model.number="target.port")
          label.field(data-field="rtmp_services[].applications[].push_targets[].application")
            span Destination application
            input(type="text" v-model="target.application" placeholder="$name")
            small Use $name for the exact source stream name.
          label.field(data-field="rtmp_services[].applications[].push_targets[].scheme")
            span Transport
            select(v-model="target.scheme")
              option(value="rtmp") RTMP
              option(value="rtmps") RTMPS
          label.field(data-field="rtmp_services[].applications[].push_targets[].stream_name")
            span Stream name override
            input(type="text" :value="target.stream_name ?? ''" @input="setNullableTargetField(target, 'stream_name', $event)")
          label.field(data-field="rtmp_services[].applications[].push_targets[].tc_url")
            span TC URL
            input(type="text" :value="target.tc_url ?? ''" @input="setNullableTargetField(target, 'tc_url', $event)")
          label.field(data-field="rtmp_services[].applications[].push_targets[].flash_version")
            span Flash version
            input(type="text" :value="target.flash_version ?? ''" @input="setNullableTargetField(target, 'flash_version', $event)")
          label.enable-row(data-field="rtmp_services[].applications[].push_targets[].credentials")
            input(type="checkbox" :checked="target.credentials !== null" @change="toggleTargetCredentials(target)")
            span Use credentials
          template(v-if="target.credentials")
            label.field(data-field="rtmp_services[].applications[].push_targets[].credentials.username")
              span Username
              input(type="text" v-model="target.credentials.username")
            label.field(data-field="rtmp_services[].applications[].push_targets[].credentials.secret_file")
              span Secret file
              input(type="text" v-model="target.credentials.secret_file" autocomplete="off")
    fieldset.object-block(data-field="rtmp_services[].applications[].vod")
      legend VOD sources
      label.enable-row
        input(type="checkbox" :checked="application.vod !== null" @change="toggleVod(application)")
        span Enable VOD playback
      template(v-if="application.vod")
        .field-grid
          label.field(data-field="rtmp_services[].applications[].vod.max_sessions")
            span Maximum VOD sessions
            input(type="number" min="1" max="100000" step="1" v-model.number="application.vod.max_sessions")
          label.field(data-field="rtmp_services[].applications[].vod.max_file_bytes")
            span Maximum VOD file bytes
            input(type="number" min="1" max="1073741824" step="1" v-model.number="application.vod.max_file_bytes")
          label.field(data-field="rtmp_services[].applications[].vod.max_duration_ms")
            span Maximum VOD duration (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.vod.max_duration_ms")
        .stack-list(data-field="rtmp_services[].applications[].vod.sources")
          article.object-block(v-for="(source, sourceIndex) in application.vod.sources" :key="sourceIndex")
            .field-grid
              label.field(data-field="rtmp_services[].applications[].vod.sources[].type")
                span Source type
                select(:value="source.type" @change="changeVodSource(application, sourceIndex, $event)")
                  option(value="local") Local files
                  option(value="http") HTTP origin
              label.field(data-field="rtmp_services[].applications[].vod.sources[].name")
                span Source name
                input(type="text" v-model="source.name")
              label.field(v-if="source.type === 'local'" data-field="rtmp_services[].applications[].vod.sources[].root_directory")
                span Root directory
                input(type="text" v-model="source.root_directory")
              label.field(v-else data-field="rtmp_services[].applications[].vod.sources[].origin")
                span HTTP origin
                input(type="url" v-model="source.origin")
              button.danger-button(type="button" @click="application.vod.sources.splice(sourceIndex, 1)") Remove source
        button.secondary-button(type="button" @click="application.vod.sources.push({ type: 'local', name: '', root_directory: '' })") Add source
    fieldset.route-list.recorder-list(data-field="rtmp_services[].applications[].recorders")
      .route-heading
        legend Recorders
        button.add-row(
          type="button"
          :disabled="!application.live || application.recorders.length >= 8"
          :title="recorderAddReason(application)"
          @click="addRecorder(applicationIndex)"
        ) + Add recorder
      p.empty-list(v-if="application.recorders.length === 0") No recorder is configured for this application.
      RtmpRecorderEditor(
        v-for="(recorder, recorderIndex) in application.recorders"
        :key="recorderIndex"
        :recorder="recorder"
        :index="recorderIndex"
        @remove="removeRecorder(applicationIndex, recorderIndex)"
      )
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type {
  RtmpAccessPolicyConfig,
  RtmpApplicationConfig,
  RtmpDashPolicyConfig,
  RtmpExecEnvironmentConfig,
  RtmpExecProfileConfig,
  RtmpHlsPolicyConfig,
  RtmpHlsVariantConfig,
  RtmpPullTargetConfig,
  RtmpPushTargetConfig,
  RtmpRecorderConfig,
  RtmpServiceConfig,
} from '../config'
import { defaultRtmpCallback, defaultRtmpRelay } from './canonicalDefaults'
import RtmpAccessPolicyEditor from './RtmpAccessPolicyEditor.vue'
import RtmpRecorderEditor from './RtmpRecorderEditor.vue'

const props = defineProps<{ service: RtmpServiceConfig }>()
const emit = defineEmits<{
  changed: []
  remove: []
}>()

function newApplication(): RtmpApplicationConfig {
  return {
    name: '',
    live: true,
    idle_streams: true,
    publish: newAccessPolicy(),
    play: newAccessPolicy(),
    limits: {
      max_connections: 1_024,
      max_publishers: 256,
      max_viewers: 1_024,
    },
    push_targets: [],
    pull_targets: [],
    relay: defaultRtmpRelay(),
    callbacks: defaultRtmpCallback(),
    fanout: {
      max_subscribers: 1_024,
      max_queue_messages_per_subscriber: 256,
      max_queue_bytes_per_subscriber: 8_388_608,
    },
    vod: null,
    hls: null,
    dash: null,
    recorders: [],
  }
}

function newHlsPolicy(): RtmpHlsPolicyConfig {
  return {
    root_directory: '/var/lib/oxiroute/hls',
    segment_duration_ms: 2_000,
    max_segment_duration_ms: 10_000,
    playlist_length_ms: 30_000,
    fragment_naming: 'sequential',
    nested: false,
    cleanup: true,
    variants: [],
    keys: null,
    max_segment_bytes: 8 * 1024 * 1024,
    max_queue_messages: 256,
    max_storage_bytes: 512 * 1024 * 1024,
    max_storage_files: 10_000,
    max_active_streams: 1_024,
  }
}

function newDashPolicy(): RtmpDashPolicyConfig {
  return {
    root_directory: '/var/lib/oxiroute/dash',
    segment_duration_ms: 5_000,
    max_segment_duration_ms: 10_000,
    playlist_length_ms: 30_000,
    segment_naming: 'sequential',
    nested: false,
    cleanup: true,
    max_segment_bytes: 8 * 1024 * 1024,
    max_queue_messages: 256,
    max_storage_bytes: 512 * 1024 * 1024,
    max_storage_files: 10_000,
    max_active_streams: 1_024,
  }
}

function newExecProfile(): RtmpExecProfileConfig {
  return {
    name: '',
    application: props.service.applications[0]?.name ?? '',
    mode: 'command',
    trigger: 'publisher',
    executable: '/usr/bin/ffmpeg',
    arguments: [],
    environment: [],
    working_directory: '/var/lib/oxiroute/exec',
    filesystem: 'working_directory',
    network: 'disabled',
    timeout_ms: 30_000,
    shutdown_timeout_ms: 5_000,
    max_processes: 1,
    max_queue_messages: 256,
    max_queue_bytes: 8 * 1024 * 1024,
    max_stdout_bytes: 1 * 1024 * 1024,
    max_stderr_bytes: 1 * 1024 * 1024,
    respawn: false,
    respawn_delay_ms: 1_000,
    max_respawns: 0,
  }
}

function newAccessPolicy(): RtmpAccessPolicyConfig {
  return { rules: [], token: null }
}

function newRecorder(): RtmpRecorderConfig {
  return {
    name: '',
    start: 'continuous',
    root_directory: '/var/lib/oxiroute/recordings',
    record_mask: { audio: true, video: true, keyframes: false },
    suffix_template: '.flv',
    append_unix_seconds: false,
    append: false,
    lock: false,
    max_size: null,
    max_frames: null,
    notify: false,
    timezone: 'utc',
    time_basis: 'segment_start',
    segment_naming: 'safe_unique',
    rotation_interval_ms: null,
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    shutdown_timeout_ms: 5_000,
    max_storage_bytes: null,
    max_storage_files: null,
    max_active_recorders: 8,
  }
}

function addApplication(): void {
  if (props.service.applications.length >= 256) return
  props.service.applications.push(newApplication())
  emit('changed')
}

function setAccessLog(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  props.service.access_log = value === 'disabled' ? { type: 'disabled' } : null
  emit('changed')
}

function toggleHls(application: RtmpApplicationConfig, event: Event): void {
  application.hls = (event.target as HTMLInputElement).checked ? newHlsPolicy() : null
}

function toggleHlsKeys(application: RtmpApplicationConfig, event: Event): void {
  if (!application.hls) return
  application.hls.keys = (event.target as HTMLInputElement).checked
    ? { rotation_segments: 5, url_prefix: '' }
    : null
}

function setNullableVariantField(
  variant: RtmpHlsVariantConfig,
  field: 'codecs' | 'width' | 'height',
  event: Event,
): void {
  const value = (event.target as HTMLInputElement).value
  if (field === 'codecs') variant.codecs = value || null
  else variant[field] = value === '' ? null : Number(value)
}

function toggleDash(application: RtmpApplicationConfig, event: Event): void {
  application.dash = (event.target as HTMLInputElement).checked ? newDashPolicy() : null
}

function addExecProfile(): void {
  if ((props.service.exec_profiles?.length ?? 0) >= 64) return
  props.service.exec_profiles ??= []
  props.service.exec_profiles.push(newExecProfile())
  emit('changed')
}

function removeExecProfile(index: number): void {
  props.service.exec_profiles?.splice(index, 1)
  emit('changed')
}

function addExecEnvironment(profile: RtmpExecProfileConfig): void {
  if (profile.environment.length >= 32) return
  const environment: RtmpExecEnvironmentConfig = { name: '', value: '' }
  profile.environment.push(environment)
  emit('changed')
}

function addPushTarget(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  if (!application?.live || application.push_targets.length >= 16) return
  application.push_targets.push({
    host: '127.0.0.1',
    port: 1_936,
    application: '$name',
    scheme: 'rtmp',
    stream_name: null,
    tc_url: null,
    flash_version: null,
    credentials: null,
  })
  emit('changed')
}

function addPullTarget(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  if (!application || application.pull_targets.length >= 16) return
  application.pull_targets.push({
    host: '127.0.0.1',
    port: 1_935,
    application: 'live',
    stream_name: '',
    scheme: 'rtmp',
    tc_url: null,
    flash_version: null,
    credentials: null,
  })
  emit('changed')
}

function removePullTarget(applicationIndex: number, targetIndex: number): void {
  props.service.applications[applicationIndex]?.pull_targets.splice(targetIndex, 1)
  emit('changed')
}

function toggleTargetCredentials(target: RtmpPushTargetConfig | RtmpPullTargetConfig): void {
  target.credentials = target.credentials === null ? { username: '', secret_file: '' } : null
}

function setNullableTargetField(
  target: RtmpPushTargetConfig | RtmpPullTargetConfig,
  field: 'tc_url' | 'flash_version',
  event: Event,
): void {
  const value = (event.target as HTMLInputElement).value
  target[field] = value || null
}

function toggleVod(application: RtmpApplicationConfig): void {
  application.vod = application.vod === null
    ? { sources: [], max_sessions: 64, max_file_bytes: 67_108_864, max_duration_ms: 21_600_000 }
    : null
}

function changeVodSource(application: RtmpApplicationConfig, index: number, event: Event): void {
  const vod = application.vod
  const source = vod?.sources[index]
  if (!source) return
  const type = (event.target as HTMLSelectElement).value
  vod.sources[index] = type === 'http'
    ? { type: 'http', name: source.name, origin: '' }
    : { type: 'local', name: source.name, root_directory: '' }
}

function removePushTarget(applicationIndex: number, targetIndex: number): void {
  props.service.applications[applicationIndex]?.push_targets.splice(targetIndex, 1)
  emit('changed')
}

function removeApplication(index: number): void {
  props.service.applications.splice(index, 1)
  emit('changed')
}

function addRecorder(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  const recorders = application?.recorders
  if (!application?.live || !recorders || recorders.length >= 8) return
  recorders.push(newRecorder())
  emit('changed')
}

function recorderAddReason(application: RtmpApplicationConfig): string | undefined {
  if (!application.live) return 'The server requires live publishing for recorder-enabled applications.'
  if (application.recorders.length >= 8) return 'The server allows at most 8 recorders per application.'
  return undefined
}

function removeRecorder(applicationIndex: number, recorderIndex: number): void {
  props.service.applications[applicationIndex]?.recorders.splice(recorderIndex, 1)
  emit('changed')
}
</script>
