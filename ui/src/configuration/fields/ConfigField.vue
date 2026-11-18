<template lang="pug">
.config-field(:data-field="path")
  label.enable-row(v-if="check" ref="root")
    slot
    span {{ label }}
  label.field(v-else ref="root")
    span {{ label }}
    slot
  FieldIssues(v-if="issues.length" :issues="issues")
</template>

<script setup lang="ts" vapor>
import { useTemplateRef } from 'vue'

import './fields.css'
import FieldIssues from './FieldIssues.vue'
import { useFieldIssues } from './context'
import { useIssueWiring } from './useIssueWiring'

const props = defineProps<{ path: string; label: string; check?: boolean }>()
const root = useTemplateRef<HTMLElement>('root')
const issues = useFieldIssues(() => props.path)
useIssueWiring(root, issues)
</script>
