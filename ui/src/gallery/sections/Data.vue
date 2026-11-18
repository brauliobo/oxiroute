<script setup vapor lang="ts">
import { ref } from 'vue'
import Badge from '#design/badge/Badge.vue'
import StatusDot from '#design/badge/StatusDot.vue'
import { TONES } from '#design/badge/tone'
import Meter from '#design/meter/Meter.vue'
import Card from '#design/panel/Card.vue'
import Panel from '#design/panel/Panel.vue'
import Sparkline from '#design/sparkline/Sparkline.vue'
import SortHeader from '#design/table/SortHeader.vue'
import Table from '#design/table/Table.vue'

const sort = ref<'ascending' | 'descending' | 'none'>('ascending')
const flip = () => (sort.value = sort.value === 'ascending' ? 'descending' : 'ascending')
const SERIES = [2, 4, 3, 6, 8, 7, 9, 12, 10, 14]
const SERVERS = [
  { name: 'origin-a', state: 'UP', tone: 'ok', rate: '1.2k/s' },
  { name: 'origin-b', state: 'DRAIN', tone: 'warn', rate: '84/s' },
  { name: 'origin-c', state: 'DOWN', tone: 'bad', rate: '0/s' },
  { name: 'origin-d', state: 'MAINT', tone: 'info', rate: '0/s' },
] as const
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-data")
  h2#demo-data Tables, badges, meters and panels
  .ox-demo-row
    Badge(v-for="tone in TONES" :key="tone" :tone="tone" :label="tone.toUpperCase()")
  .ox-demo-row
    span(v-for="tone in TONES" :key="tone")
      StatusDot(:tone="tone")
      |  {{ tone }}
  Table(caption="Servers: name, state and request rate")
    template(v-slot:head)
      tr
        SortHeader(label="Server" sortable :direction="sort" @sort="flip")
        SortHeader(label="State")
        SortHeader(label="Req/s")
    tbody(v-for="server in SERVERS" :key="server.name")
      tr
        th.ox-name(scope="row" data-label="Server") {{ server.name }}
        td(data-label="State")
          Badge(:tone="server.tone" :label="server.state")
        td(data-label="Req/s") {{ server.rate }}
  .ox-demo-grid
    Panel(title="Load")
      Meter(:value="42" label="Connections" value-text="42 of 100")
      Meter(:value="85" label="Memory" tone="warn" value-text="85 percent")
      Meter(:value="97" label="Disk" tone="bad" value-text="97 percent")
      Sparkline(:series="SERIES" label="Requests per second")
    Card
      p A card is a bordered surface for grouped controls.
</template>
