<script setup vapor lang="ts">
import { computed, ref, useId } from 'vue'
import Chip from '#design/button/Chip.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import type { ServerInventoryEntry } from '../api'
import type { ServerCommand } from './commands'
import ForceToggle from './ForceToggle.vue'
import { plural } from './labels'
import { filterServers } from './serverFilter'
import ServerCard from './ServerCard.vue'
import './page.css'

const props = defineProps<{ servers: readonly ServerInventoryEntry[]; force: boolean; disabled: boolean }>()
const emit = defineEmits<{ command: [command: ServerCommand]; 'update:force': [force: boolean] }>()

const searchId = useId()
const query = ref('')
const problemsOnly = ref(false)
const visible = computed(() => filterServers(props.servers, query.value, problemsOnly.value))
const count = computed(() => `Showing ${visible.value.length} of ${plural(props.servers.length, 'server')}.`)

const toggleProblems = () => { problemsOnly.value = !problemsOnly.value }
</script>

<template lang="pug">
EmptyState(v-if="servers.length === 0" title="No servers") No upstream servers are configured.
template(v-else)
  .ox-ops-toolbar
    Field(label="Search servers" :label-for="searchId")
      Input(:id="searchId" v-model="query" type="search" placeholder="Name, pool or address")
    Chip(:pressed="problemsOnly" @click="toggleProblems") Problems only
    ForceToggle(:force="force" @update:force="(next) => emit('update:force', next)")
  p.ox-muted(role="status") {{ count }}
  EmptyState(v-if="visible.length === 0" title="No server matches") Change the search or turn off Problems only.
  .ox-ops-server-list(v-else)
    ServerCard(
      v-for="entry in visible"
      :key="`${entry.pool}:${entry.server.name}`"
      :entry="entry"
      :disabled="disabled"
      @command="(command) => emit('command', command)"
    )
</template>
