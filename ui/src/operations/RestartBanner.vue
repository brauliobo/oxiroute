<script setup vapor lang="ts">
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import { formatTimestamp } from '#design/format'
import type { RestartRequired } from '../api/generated/controlPlane'
import { plural } from './labels'
import './page.css'

defineProps<{ restart: RestartRequired }>()
</script>

<template lang="pug">
Notice(kind="status")
  template(v-if="restart.required")
    Banner(tone="warn")
      strong Restart required.
      |  {{ plural(restart.reasons.length, 'setting') }} in the saved configuration only {{ restart.reasons.length === 1 ? 'takes' : 'take' }} effect after the process restarts.
      span(v-if="restart.since !== null")  Waiting since {{ formatTimestamp(restart.since) }}.
    ul.ox-ops-detail(data-restart-reasons)
      li(v-for="reason in restart.reasons" :key="`${reason.code}:${reason.path}`")
        code {{ reason.path }}
        |  {{ reason.summary }}
</template>
