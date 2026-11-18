<script setup vapor lang="ts">
import { onMounted, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Table from '#design/table/Table.vue'
import ScrollRegion from '../../workspace/ScrollRegion.vue'
import { ROLE_LABELS, useFingerprints } from './useFingerprints'
import './page.css'

const COLUMNS = ['Service', 'Application', 'Role', 'Fingerprint']

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const { fingerprints, error, loading, load, reset } = useFingerprints(() => props.token, () => emit('unauthorized'))

watch(() => props.token, (token) => {
  reset()
  if (token) void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>

<template lang="pug">
section.ox-keys(aria-label="Access keys" :aria-busy="loading")
  header.ox-keys-heading
    div
      p.ox-muted The server never shows the keys themselves, only a fingerprint: "fp-" followed by the first 8 hex digits of the key's SHA-256, the same value the callbacks receive in the query field. To check which key an application uses, compare its fingerprint with the one you expect.
      p.ox-muted A short fingerprint can let someone who already holds the management token confirm a guessed weak key, so use long random keys.
    Button(data-action="refresh-keys" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh' }}

  Notice(kind="alert")
    Banner(v-if="error" tone="bad" data-keys-error)
      strong Keys unavailable.
      |  {{ error }}
      template(v-slot:actions)
        Button(small data-action="retry-keys" :disabled="loading" @click="load") Retry

  Loading(v-if="loading && !fingerprints" label="Loading key fingerprints")

  template(v-if="fingerprints")
    EmptyState(v-if="fingerprints.length === 0" title="No application requires a publish or play key")
      | Applications without a key accept any publisher and any player, so there is nothing to compare.
    ScrollRegion(v-else label="Access key fingerprints")
      Table(caption="Publish and play keys by service and application, shown as fingerprints" :columns="COLUMNS")
        tbody
          tr(
            v-for="entry in fingerprints"
            :key="`${entry.service}/${entry.application}/${entry.role}`"
            :data-key-entry="`${entry.service}/${entry.application}/${entry.role}`"
          )
            th.ox-name(scope="row" data-label="Service") {{ entry.service }}
            td(data-label="Application") {{ entry.application }}
            td(data-label="Role") {{ ROLE_LABELS[entry.role] }}
            td(data-label="Fingerprint")
              code.ox-mono(data-fingerprint) {{ entry.fingerprint }}
</template>
