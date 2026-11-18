<template lang="pug">
.field-issues
  p.field-error(v-for="issue in issues" :id="issue.id" :key="issue.id" :data-severity="issue.diagnostic.severity")
    strong {{ issue.diagnostic.severity === 'error' ? 'Error:' : 'Warning:' }}
    |  {{ issue.diagnostic.message }}
    span.field-error-detail(v-if="issue.diagnostic.detail") {{ issue.diagnostic.detail }}
    code.field-error-path(v-if="issue.diagnostic.path") {{ issue.diagnostic.path }}
</template>

<script setup lang="ts" vapor>
import type { PlacedDiagnostic } from './context'

defineProps<{ issues: PlacedDiagnostic[] }>()
</script>
