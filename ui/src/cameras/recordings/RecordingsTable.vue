<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import { UNKNOWN } from '#design/format'
import SortHeader from '#design/table/SortHeader.vue'
import Table from '#design/table/Table.vue'
import type { Recording } from '../../api'
import { ACTIVE_REASON } from './plans'
import { ariaSort, SORT_KEYS, SORT_LABELS, type SortKey, type SortState } from './sort'
import { STATUS_WORDS } from './status'
import { codecText, durationText, sizeText, startedText } from './text'
import './page.css'

defineProps<{ recordings: readonly Recording[]; sort: SortState; busy: boolean }>()
const emit = defineEmits<{ sort: [key: SortKey]; download: [recording: Recording]; remove: [recording: Recording] }>()
</script>

<template lang="pug">
Table(caption="Recordings: name, stream, start, duration, size, status, codecs and actions")
  template(v-slot:head)
    tr
      SortHeader(
        v-for="key in SORT_KEYS"
        :key="key"
        sortable
        :label="SORT_LABELS[key]"
        :direction="ariaSort(sort, key)"
        @sort="emit('sort', key)"
      )
      SortHeader(label="Codecs")
      SortHeader(label="Actions")
  tbody
    tr(v-for="recording in recordings" :key="recording.id" :data-recording-id="recording.id")
      th.ox-name(scope="row" data-label="Name")
        span.ox-mono {{ recording.name }}
      td(data-label="Stream") {{ recording.stream ?? UNKNOWN }}
      td(data-label="Started") {{ startedText(recording) }}
      td(data-label="Duration") {{ durationText(recording) }}
      td(data-label="Size") {{ sizeText(recording) }}
      td(data-label="Status" :data-status="recording.status")
        Badge(:tone="STATUS_WORDS[recording.status].tone" :label="STATUS_WORDS[recording.status].label")
        span.ox-recs-note(v-if="STATUS_WORDS[recording.status].note") {{ STATUS_WORDS[recording.status].note }}
      td(data-label="Codecs") {{ codecText(recording) }}
      td(data-label="Actions")
        .ox-recs-actions
          Button(data-action="download" :aria-label="`Download ${recording.name}`" :disabled="busy" @click="emit('download', recording)") Download
          Button(
            variant="danger"
            data-action="delete"
            :aria-label="`Delete ${recording.name}`"
            :aria-describedby="recording.status === 'recording' ? `${recording.id}-reason` : undefined"
            :disabled="busy || recording.status === 'recording'"
            @click="emit('remove', recording)"
          ) Delete
        span.ox-recs-note(v-if="recording.status === 'recording'" :id="`${recording.id}-reason`") {{ ACTIVE_REASON }}
</template>
