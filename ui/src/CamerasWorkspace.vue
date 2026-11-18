<script setup lang="ts" vapor>
import { ref } from 'vue'
import Tabs from '#design/tabs/Tabs.vue'
import type { RtmpCatalog, RtmpStats } from './api'
import KeysPanel from './cameras/keys/KeysPanel.vue'
import RecordingsPanel from './cameras/recordings/RecordingsPanel.vue'
import StreamsPanel from './cameras/streams/StreamsPanel.vue'
import WorkspaceSection from './workspace/WorkspaceSection.vue'

const TABS = [
  { id: 'streams', label: 'Streams' },
  { id: 'recordings', label: 'Recordings' },
  { id: 'keys', label: 'Access keys' },
]

defineProps<{
  token: string
  catalog: RtmpCatalog | null
  stats: RtmpStats | null
  catalogError: string | null
  statsError: string | null
  nowUnixMs: number
  refreshing: boolean
  reload: () => Promise<void>
}>()
const emit = defineEmits<{ unauthorized: [] }>()

const tab = ref('streams')
const unauthorized = () => emit('unauthorized')
</script>

<template lang="pug">
WorkspaceSection(
  title="Cameras and recordings"
  description="Every RTMP stream with its picture, health and publisher, the recordings kept on disk, and the keys the server expects, shown as fingerprints."
)
  Tabs(:tabs="TABS" v-model="tab" label="Camera sections")
    StreamsPanel(
      v-if="tab === 'streams'"
      :token="token"
      :catalog="catalog"
      :stats="stats"
      :catalog-error="catalogError"
      :stats-error="statsError"
      :now-unix-ms="nowUnixMs"
      :refreshing="refreshing"
      :reload="reload"
      @unauthorized="unauthorized"
    )
    RecordingsPanel(v-else-if="tab === 'recordings'" :token="token" @unauthorized="unauthorized")
    KeysPanel(v-else :token="token" @unauthorized="unauthorized")
</template>
