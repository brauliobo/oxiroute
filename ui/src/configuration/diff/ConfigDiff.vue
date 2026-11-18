<template lang="pug">
section.ox-diff(aria-labelledby="ox-diff-heading")
  h3#ox-diff-heading Changes to the rendered configuration
  p.ox-diff-summary(v-if="changed === 0") No line changes: the candidate renders exactly like the file on disk.
  p.ox-diff-summary(v-else) {{ added }} {{ added === 1 ? 'line' : 'lines' }} added (+), {{ removed }} {{ removed === 1 ? 'line' : 'lines' }} removed (-)
  .ox-diff-scroll(v-if="hunks.length" tabindex="0" role="region" aria-label="Configuration diff")
    template(v-for="(hunk, hunkIndex) in hunks" :key="hunkIndex")
      p.ox-diff-gap(v-if="hunkIndex > 0") Unchanged lines hidden
      div.ox-diff-line(v-for="(line, lineIndex) in hunk.lines" :key="lineIndex" :data-kind="line.kind")
        span.ox-diff-number {{ line.oldLine ?? '' }}
        span.ox-diff-number {{ line.newLine ?? '' }}
        span.ox-diff-mark(aria-hidden="true") {{ marks[line.kind] }}
        span.ox-sr-only {{ words[line.kind] }}
        code {{ line.text }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import './styles.css'
import { changedLineCount, diffHunks, type DiffKind, type DiffLine } from './lines'

const props = defineProps<{ lines: DiffLine[] }>()
const marks: Record<DiffKind, string> = { same: ' ', added: '+', removed: '-' }
const words: Record<DiffKind, string> = { same: 'Unchanged: ', added: 'Added: ', removed: 'Removed: ' }
const hunks = computed(() => diffHunks(props.lines))
const changed = computed(() => changedLineCount(props.lines))
const added = computed(() => props.lines.filter(({ kind }) => kind === 'added').length)
const removed = computed(() => changed.value - added.value)
</script>
