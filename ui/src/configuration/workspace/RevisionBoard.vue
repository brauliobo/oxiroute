<template lang="pug">
Card.revision-board(aria-label="Configuration revisions")
  .revision-cell
    span.revision-label Disk revision
    code {{ shortRevision(diskRevision) }}
    span.revision-detail Save precondition
  .revision-cell
    span.revision-label Active revision
    code {{ activeRevision ? shortRevision(activeRevision) : 'None' }}
    span.revision-detail Serving live traffic
  .revision-cell
    span.revision-label Draft state
    strong(:class="{ changed: dirty }") {{ dirty ? 'Unsaved changes' : 'In sync with disk' }}
    span.revision-detail {{ validationCurrent ? 'Server validated' : 'Validation required' }}
</template>

<script setup lang="ts" vapor>
import Card from '#design/panel/Card.vue'
import { shortRevision } from '../../formatters'

defineProps<{
  diskRevision: string
  activeRevision: string | null
  dirty: boolean
  validationCurrent: boolean
}>()
</script>
