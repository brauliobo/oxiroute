<template lang="pug">
strong {{ diagnostic.message }}
p.diagnostic-detail(v-if="diagnostic.detail") {{ diagnostic.detail }}
p.diagnostic-meta {{ meta }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import type { ConfigDiagnostic } from '../../config'

const props = defineProps<{ diagnostic: ConfigDiagnostic }>()
const meta = computed(() =>
  [props.diagnostic.stage, props.diagnostic.line ? `line ${props.diagnostic.line}` : '', props.diagnostic.path ?? '']
    .filter(Boolean)
    .join(' / '),
)
</script>
