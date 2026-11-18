<script setup vapor lang="ts">
import { computed, ref, watch } from 'vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'
import type { AuditCategory, AuditResult } from '../api'
import { CATEGORY_OPTIONS, RESULT_OPTIONS } from './model'
import type { AuditFilterSelection } from './types'

const props = defineProps<{ applied: AuditFilterSelection; disabled: boolean }>()
const emit = defineEmits<{ apply: [filters: AuditFilterSelection]; clear: [] }>()

const category = ref<AuditFilterSelection['category']>(props.applied.category)
const result = ref<AuditFilterSelection['result']>(props.applied.result)
const clearable = computed(() => Boolean(category.value || result.value || props.applied.category || props.applied.result))

watch(() => props.applied, (applied) => {
  category.value = applied.category
  result.value = applied.result
})

const apply = () => emit('apply', { category: category.value, result: result.value })
const setCategory = (value: string) => { category.value = value as AuditCategory | '' }
const setResult = (value: string) => { result.value = value as AuditResult | '' }
</script>

<template lang="pug">
form.ox-toolbar(@submit.prevent="apply")
  Field.ox-filter(label="Category" label-for="audit-category")
    Select#audit-category(:model-value="category" :options="CATEGORY_OPTIONS" :disabled="disabled" @update:modelValue="setCategory")
  Field.ox-filter(label="Result" label-for="audit-result")
    Select#audit-result(:model-value="result" :options="RESULT_OPTIONS" :disabled="disabled" @update:modelValue="setResult")
  Button(type="submit" variant="primary" :disabled="disabled") Apply filters
  Button(:disabled="disabled || !clearable" @click="emit('clear')") Clear filters
</template>
