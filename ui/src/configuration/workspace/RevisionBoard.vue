<template lang="pug">
section.revision-board(:inert="reviewOpen || undefined" aria-label="Configuration revisions")
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
import { shortRevision } from '../../formatters'

defineProps<{
  diskRevision: string
  activeRevision: string | null
  dirty: boolean
  validationCurrent: boolean
  reviewOpen: boolean
}>()
</script>

<style scoped src="./base.css"></style>
<style scoped>
.revision-board {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  margin-top: 30px;
  border: 1px solid #3a4034;
  background: #121510;
}

.revision-cell {
  display: grid;
  min-width: 0;
  gap: 7px;
  padding: 17px 19px;
}

.revision-cell + .revision-cell {
  border-left: 1px solid #34392f;
}

.revision-cell code,
.revision-cell strong {
  overflow: hidden;
  color: #dce3d4;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.85rem;
  text-overflow: ellipsis;
}

.revision-cell strong.changed {
  color: #ffcf70;
}

.revision-detail {
  color: #70786a;
  font-size: 0.7rem;
}

@media (max-width: 700px) {
  .revision-board {
    grid-template-columns: 1fr;
  }

  .revision-cell + .revision-cell {
    border-top: 1px solid #34392f;
    border-left: 0;
  }
}
</style>
