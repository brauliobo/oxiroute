<script setup vapor lang="ts">
import { computed } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Card from '#design/panel/Card.vue'
import Meter from '#design/meter/Meter.vue'
import type { RecordingRoot } from '../../api'
import { quotaAlerts, retentionLines, rootName, usages } from './quota'
import './page.css'

const props = defineProps<{ roots: readonly RecordingRoot[] }>()

const alerts = computed(() => quotaAlerts(props.roots))
const cards = computed(() => props.roots.map((root) => ({
  name: rootName(root),
  service: root.recorders[0]?.service ?? '',
  usages: usages(root),
  retention: retentionLines(root),
})))
</script>

<template lang="pug">
Notice(kind="alert")
  Banner(v-for="alert in alerts" :key="alert.name" :tone="alert.tone" :data-quota-alert="alert.name")
    strong Recording space is running out.
    |  {{ alert.text }}
.ox-recs-quota
  Card.ox-recs-root(v-for="card in cards" :key="card.name" :data-root="card.name")
    h3 {{ card.name }}
    p.ox-muted server {{ card.service }}
    .ox-recs-usage(v-for="usage in card.usages" :key="usage.key" :data-usage="usage.key")
      strong {{ usage.label }}
      Meter(
        v-if="usage.limited"
        :value="usage.used"
        :max="usage.max"
        :tone="usage.tone"
        :label="`${card.name} ${usage.label.toLowerCase()} used`"
        :value-text="usage.text"
      )
      span.ox-recs-usage-text {{ usage.text }}
    ul.ox-recs-retention
      li(v-for="line in card.retention" :key="line") {{ line }}
</template>
