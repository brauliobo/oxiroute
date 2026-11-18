<template lang="pug">
ConfigGroup.route-list(path="rtmp_services[].exec_profiles")
  template(#legend)
    .route-heading
      legend Exec profiles
      button.add-row(
        type="button"
        :disabled="(service.exec_profiles?.length ?? 0) >= 64"
        @click="emit('add')"
      ) + Add exec profile
  p.empty-list(v-if="!service.exec_profiles || service.exec_profiles.length === 0") No exec profile is configured for this service.
  ItemScope.route-card(v-for="(profile, profileIndex) in service.exec_profiles || []" :key="profileIndex" :index="profileIndex")
    header.route-card-heading
      strong Exec profile {{ profileIndex + 1 }}
      button.danger-link(type="button" @click="emit('remove', profileIndex)") Remove
    .field-grid
      ConfigField(path="rtmp_services[].exec_profiles[].name" label="Profile name")
        input(type="text" v-model="profile.name")
      ConfigField(path="rtmp_services[].exec_profiles[].application" label="Application")
        select(v-model="profile.application")
          option(v-for="application in service.applications" :key="application.name" :value="application.name") {{ application.name || 'Unnamed application' }}
      ConfigField(path="rtmp_services[].exec_profiles[].mode" label="Exec mode")
        select(v-model="profile.mode")
          option(value="command") Command
          option(value="transcode") Transcode
      ConfigField(path="rtmp_services[].exec_profiles[].trigger" label="Trigger")
        select(v-model="profile.trigger")
          option(value="publisher") Publisher
          option(value="publish_done") Publish done
      ConfigField(path="rtmp_services[].exec_profiles[].executable" label="Executable")
        input(type="text" v-model="profile.executable" placeholder="/usr/bin/ffmpeg")
      StringListField(
        v-model="profile.arguments"
        label="Arguments"
        item-label="argument"
        field-path="rtmp_services[].exec_profiles[].arguments"
        :max-items="64"
      )
      ConfigField(path="rtmp_services[].exec_profiles[].working_directory" label="Working directory")
        input(type="text" v-model="profile.working_directory" placeholder="/var/lib/oxiroute/exec")
      ConfigField(path="rtmp_services[].exec_profiles[].filesystem" label="Filesystem policy")
        select(v-model="profile.filesystem")
          option(value="working_directory") Working directory only
          option(value="host") Host (rejected by runtime)
      ConfigField(path="rtmp_services[].exec_profiles[].network" label="Network policy")
        select(v-model="profile.network")
          option(value="disabled") Disabled
          option(value="inherited") Inherited
      ConfigField(path="rtmp_services[].exec_profiles[].timeout_ms" label="Timeout (ms)")
        input(type="number" min="1" max="86400000" step="1" v-model.number="profile.timeout_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].shutdown_timeout_ms" label="Shutdown timeout (ms)")
        input(type="number" min="1" max="60000" step="1" v-model.number="profile.shutdown_timeout_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].max_processes" label="Maximum processes")
        input(type="number" min="1" max="256" step="1" v-model.number="profile.max_processes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_queue_messages" label="Maximum queue messages")
        input(type="number" min="1" max="65536" step="1" v-model.number="profile.max_queue_messages")
      ConfigField(path="rtmp_services[].exec_profiles[].max_queue_bytes" label="Maximum queue bytes")
        input(type="number" min="1" max="1073741824" step="1" v-model.number="profile.max_queue_bytes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_stdout_bytes" label="Maximum stdout bytes")
        input(type="number" min="1" max="16777216" step="1" v-model.number="profile.max_stdout_bytes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_stderr_bytes" label="Maximum stderr bytes")
        input(type="number" min="1" max="16777216" step="1" v-model.number="profile.max_stderr_bytes")
      ConfigField.compact-enable(path="rtmp_services[].exec_profiles[].respawn" label="Respawn on exit" check)
        input(type="checkbox" v-model="profile.respawn")
      ConfigField(path="rtmp_services[].exec_profiles[].respawn_delay_ms" label="Respawn delay (ms)")
        input(type="number" min="1" max="300000" step="1" v-model.number="profile.respawn_delay_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].max_respawns" label="Maximum respawns")
        input(type="number" min="0" max="64" step="1" v-model.number="profile.max_respawns")
    ConfigGroup.object-block(path="rtmp_services[].exec_profiles[].environment" legend="Environment")
      button.secondary-button(type="button" :disabled="profile.environment.length >= 32" @click="emit('add-environment', profile)") + Add variable
      p.empty-list(v-if="profile.environment.length === 0") No environment variables are configured.
      ItemScope.field-grid(v-for="(environment, environmentIndex) in profile.environment" :key="environmentIndex" :index="environmentIndex")
        ConfigField(path="rtmp_services[].exec_profiles[].environment[].name" label="Name")
          input(type="text" v-model="environment.name")
        ConfigField(path="rtmp_services[].exec_profiles[].environment[].value" label="Value")
          input(type="text" v-model="environment.value" autocomplete="off")
        button.danger-link(type="button" @click="profile.environment.splice(environmentIndex, 1)") Remove
</template>

<script setup lang="ts" vapor>
import StringListField from '../StringListField.vue'
import type { RtmpExecProfileConfig, RtmpServiceConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

defineProps<{ service: RtmpServiceConfig }>()
const emit = defineEmits<{
  add: []
  remove: [index: number]
  'add-environment': [profile: RtmpExecProfileConfig]
}>()
</script>
