<template lang="pug">
.config-field(ref="root" :data-field="path")
  Checkbox.enable-row(v-if="check && !hasControl" :model-value="modelValue ?? false" :disabled="disabled" @update:model-value="emit('update:modelValue', $event)") {{ label }}
  label.enable-row(v-else-if="check")
    slot
    span {{ label }}
  label.field(v-else)
    span {{ label }}
    slot
  FieldIssues(v-if="issues.length" :issues="issues")
</template>

<script setup lang="ts" vapor>
import { useSlots, useTemplateRef } from 'vue'

import Checkbox from '#design/field/Checkbox.vue'
import './fields.css'
import FieldIssues from './FieldIssues.vue'
import { useFieldIssues } from './context'
import { useIssueWiring } from './useIssueWiring'

const props = defineProps<{ path: string; label: string; check?: boolean; modelValue?: boolean; disabled?: boolean }>()
const hasControl = useSlots().default !== undefined
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
const root = useTemplateRef<HTMLElement>('root')
const issues = useFieldIssues(() => props.path)
useIssueWiring(root, issues)
</script>
