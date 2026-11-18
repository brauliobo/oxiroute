<template lang="pug">
fieldset(ref="root" :data-field="path")
  slot(name="legend")
    legend(v-if="legend") {{ legend }}
  FieldIssues(v-if="issues.length" :issues="issues")
  slot
</template>

<script setup lang="ts" vapor>
import { useTemplateRef } from 'vue'

import './fields.css'
import FieldIssues from './FieldIssues.vue'
import { useFieldIssues } from './context'
import { useIssueWiring } from './useIssueWiring'

const props = defineProps<{ path: string; legend?: string }>()
const root = useTemplateRef<HTMLElement>('root')
const issues = useFieldIssues(() => props.path)
useIssueWiring(root, issues, true)
</script>
