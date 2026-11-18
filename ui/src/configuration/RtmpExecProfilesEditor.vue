<template lang="pug">
ConfigGroup.route-list(path="rtmp_services[].exec_profiles")
  template(#legend)
    .route-heading
      legend Exec profiles
      Button.add-row(
        :disabled="(service.exec_profiles?.length ?? 0) >= 64"
        @click="emit('add')"
      ) + Add exec profile
  p.empty-list(v-if="!service.exec_profiles || service.exec_profiles.length === 0") No exec profile is configured for this service.
  ItemScope.route-card(v-for="(profile, profileIndex) in service.exec_profiles || []" :key="profileIndex" :index="profileIndex")
    header.route-card-heading
      strong Exec profile {{ profileIndex + 1 }}
      Button.danger-link(variant="danger" small @click="emit('remove', profileIndex)") Remove
    .field-grid
      ConfigField(path="rtmp_services[].exec_profiles[].name" label="Profile name")
        Input(v-model="profile.name")
      ConfigField(path="rtmp_services[].exec_profiles[].application" label="Application")
        select.ox-input(v-model="profile.application")
          option(v-for="application in service.applications" :key="application.name" :value="application.name") {{ application.name || 'Unnamed application' }}
      ConfigField(path="rtmp_services[].exec_profiles[].mode" label="Exec mode")
        Select(
          v-model="profile.mode"
          :options="[{ value: 'command', label: 'Command' }, { value: 'transcode', label: 'Transcode' }]"
        )
      ConfigField(path="rtmp_services[].exec_profiles[].trigger" label="Trigger")
        Select(
          v-model="profile.trigger"
          :options="[{ value: 'publisher', label: 'Publisher' }, { value: 'publish_done', label: 'Publish done' }]"
        )
      ConfigField(path="rtmp_services[].exec_profiles[].executable" label="Executable")
        Input(v-model="profile.executable" placeholder="/usr/bin/ffmpeg")
      StringListField(
        v-model="profile.arguments"
        label="Arguments"
        item-label="argument"
        field-path="rtmp_services[].exec_profiles[].arguments"
        :max-items="64"
      )
      ConfigField(path="rtmp_services[].exec_profiles[].working_directory" label="Working directory")
        Input(v-model="profile.working_directory" placeholder="/var/lib/oxiroute/exec")
      ConfigField(path="rtmp_services[].exec_profiles[].filesystem" label="Filesystem policy")
        Select(
          v-model="profile.filesystem"
          :options="[{ value: 'working_directory', label: 'Working directory only' }, { value: 'host', label: 'Host (rejected by runtime)' }]"
        )
      ConfigField(path="rtmp_services[].exec_profiles[].network" label="Network policy")
        Select(
          v-model="profile.network"
          :options="[{ value: 'disabled', label: 'Disabled' }, { value: 'inherited', label: 'Inherited' }]"
        )
      ConfigField(path="rtmp_services[].exec_profiles[].timeout_ms" label="Timeout (ms)")
        NumberInput(min="1" max="86400000" step="1" v-model="profile.timeout_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].shutdown_timeout_ms" label="Shutdown timeout (ms)")
        NumberInput(min="1" max="60000" step="1" v-model="profile.shutdown_timeout_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].max_processes" label="Maximum processes")
        NumberInput(min="1" max="256" step="1" v-model="profile.max_processes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_queue_messages" label="Maximum queue messages")
        NumberInput(min="1" max="65536" step="1" v-model="profile.max_queue_messages")
      ConfigField(path="rtmp_services[].exec_profiles[].max_queue_bytes" label="Maximum queue bytes")
        NumberInput(min="1" max="1073741824" step="1" v-model="profile.max_queue_bytes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_stdout_bytes" label="Maximum stdout bytes")
        NumberInput(min="1" max="16777216" step="1" v-model="profile.max_stdout_bytes")
      ConfigField(path="rtmp_services[].exec_profiles[].max_stderr_bytes" label="Maximum stderr bytes")
        NumberInput(min="1" max="16777216" step="1" v-model="profile.max_stderr_bytes")
      ConfigField.compact-enable(path="rtmp_services[].exec_profiles[].respawn" label="Respawn on exit" check v-model="profile.respawn")
      ConfigField(path="rtmp_services[].exec_profiles[].respawn_delay_ms" label="Respawn delay (ms)")
        NumberInput(min="1" max="300000" step="1" v-model="profile.respawn_delay_ms")
      ConfigField(path="rtmp_services[].exec_profiles[].max_respawns" label="Maximum respawns")
        NumberInput(min="0" max="64" step="1" v-model="profile.max_respawns")
    ConfigGroup.object-block(path="rtmp_services[].exec_profiles[].environment" legend="Environment")
      Button.secondary-button(:disabled="profile.environment.length >= 32" @click="emit('add-environment', profile)") + Add variable
      p.empty-list(v-if="profile.environment.length === 0") No environment variables are configured.
      ItemScope.field-grid(v-for="(environment, environmentIndex) in profile.environment" :key="environmentIndex" :index="environmentIndex")
        ConfigField(path="rtmp_services[].exec_profiles[].environment[].name" label="Name")
          Input(v-model="environment.name")
        ConfigField(path="rtmp_services[].exec_profiles[].environment[].value" label="Value")
          Input(v-model="environment.value" autocomplete="off")
        Button.danger-link(variant="danger" small @click="profile.environment.splice(environmentIndex, 1)") Remove
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
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
