<template lang="pug">
footer.editor-actions
  Button.secondary-button(:disabled="!dirty || validating || saving" :title="resetDisabledReason" @click="emit('reset')") Reset draft
  Button.primary-button(variant="primary" :disabled="validating || saving || stale" :title="validationDisabledReason" @click="emit('validate')")
    | {{ validating ? 'Validating...' : 'Validate candidate' }}
  span.diagnostic-summary(v-if="summary") {{ summary }}
  Button.review-button(variant="primary" :disabled="!canReview || saving" :title="reviewDisabledReason" @click="emit('review')") Review save
</template>

<script setup lang="ts" vapor>
import Button from '#design/button/Button.vue'
import { computed } from 'vue'

const props = defineProps<{
  dirty: boolean
  validating: boolean
  saving: boolean
  stale: boolean
  compositional: boolean
  validationCurrent: boolean
  canReview: boolean
  summary: string
}>()
const emit = defineEmits<{ reset: []; validate: []; review: [] }>()

const resetDisabledReason = computed(() => {
  if (props.validating || props.saving) return 'Wait for the current configuration request to finish.'
  if (!props.dirty) return 'The draft already matches the disk snapshot.'
  return undefined
})
const validationDisabledReason = computed(() => {
  if (props.stale) return 'Reload the changed disk revision before validating this draft.'
  if (props.validating || props.saving) return 'Wait for the current configuration request to finish.'
  return undefined
})
const reviewDisabledReason = computed(() => {
  if (props.saving) return 'A configuration save is in progress.'
  if (props.compositional) return 'Typed saves cannot replace a compositional configuration root.'
  if (props.stale) return 'Reload the changed disk revision before reviewing a save.'
  if (!props.validationCurrent) return 'Validate the current draft on the server before reviewing a save.'
  return undefined
})
</script>
