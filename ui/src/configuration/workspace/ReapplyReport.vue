<template lang="pug">
section.reapply-report(aria-labelledby="reapply-heading")
  header
    h3#reapply-heading Your edits on the new disk version
    Button(small @click="emit('dismiss')") Dismiss
  p {{ summary }}
  template(v-if="report.applied.length")
    h4 Reapplied ({{ report.applied.length }})
    ul
      li(v-for="change in report.applied" :key="change.path")
        code {{ change.path }}
        |  {{ formatValue(change.before) }} to {{ formatValue(change.after) }}
  template(v-if="report.collisions.length")
    h4 Kept the disk value ({{ report.collisions.length }})
    ul
      li(v-for="collision in report.collisions" :key="collision.path")
        code {{ collision.path }}
        |  disk {{ formatValue(collision.theirs) }}, yours {{ formatValue(collision.mine) }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import Button from '#design/button/Button.vue'
import { formatValue } from '../merge/summary'
import type { MergeResult } from '../merge/threeWay'
import './reapply.css'

const props = defineProps<{ report: Pick<MergeResult<unknown>, 'applied' | 'collisions'> }>()
const emit = defineEmits<{ dismiss: [] }>()
const plural = (count: number): string => `${count} ${count === 1 ? 'change' : 'changes'}`
const summary = computed(() => props.report.collisions.length === 0
  ? `Reapplied ${plural(props.report.applied.length)}; nothing collided.`
  : `Reapplied ${plural(props.report.applied.length)}; ${plural(props.report.collisions.length)} collided with the new disk version, which was kept.`)
</script>
