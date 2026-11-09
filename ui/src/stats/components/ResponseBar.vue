<script setup vapor lang="ts">
import { computed } from 'vue'
import { vCssVars } from '../../cssVars'
import { count, exact, responseShares } from './present'
import type { ResponseClass } from './present'
import './widgets.css'

// Response classes as one stacked bar and, beside it, the numbers: the bar shows proportion, the text the value.
const props = defineProps<{ responses: Readonly<Record<ResponseClass, bigint>> }>()
const shares = computed(() => responseShares(props.responses))
const present = computed(() => shares.value.filter((entry) => entry.count > 0n))
const title = computed(() => present.value.map((entry) => `${entry.class}: ${exact(entry.count)}`).join(', '))
const width = (share: number) => ({ '--ox-w': share })
</script>

<template lang="pug">
.ox-resp(v-if="present.length" :title="title")
  .ox-resp-bar(aria-hidden="true")
    span.ox-resp-seg(v-for="entry in present" :key="entry.class" :class="`ox-r${entry.class}`" v-css-vars="width(entry.share)")
  ul.ox-resp-nums
    li(v-for="entry in present" :key="entry.class" :class="`ox-r${entry.class}`")
      span.ox-resp-name {{ entry.class }}
      |  {{ count(entry.count) }}
span.ox-muted(v-else) none yet
</template>
