<template lang="pug">
fieldset.route-list(data-field="rtmp_services[].exec_profiles")
  .route-heading
    legend Exec profiles
    button.add-row(
      type="button"
      :disabled="(service.exec_profiles?.length ?? 0) >= 64"
      @click="$emit('add')"
    ) + Add exec profile
  p.empty-list(v-if="!service.exec_profiles || service.exec_profiles.length === 0") No exec profile is configured for this service.
  article.route-card(v-for="(profile, profileIndex) in service.exec_profiles || []" :key="profileIndex")
    header.route-card-heading
      strong Exec profile {{ profileIndex + 1 }}
      button.danger-link(type="button" @click="$emit('remove', profileIndex)") Remove
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
      button.secondary-button(type="button" :disabled="profile.environment.length >= 32" @click="$emit('add-environment', profile)") + Add variable
      p.empty-list(v-if="profile.environment.length === 0") No environment variables are configured.
      .field-grid(v-for="(environment, environmentIndex) in profile.environment" :key="environmentIndex")
        label.field(data-field="rtmp_services[].exec_profiles[].environment[].name")
          span Name
          input(type="text" v-model="environment.name")
        label.field(data-field="rtmp_services[].exec_profiles[].environment[].value")
          span Value
          input(type="text" v-model="environment.value" autocomplete="off")
        button.danger-link(type="button" @click="profile.environment.splice(environmentIndex, 1)") Remove
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type { RtmpExecProfileConfig, RtmpServiceConfig } from '../config'

defineProps<{ service: RtmpServiceConfig }>()
defineEmits<{
  add: []
  remove: [index: number]
  'add-environment': [profile: RtmpExecProfileConfig]
}>()
</script>
