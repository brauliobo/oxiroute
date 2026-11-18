<template lang="pug">
Dialog.ox-review(:open="open" title="Save review" @close="emit('close')")
  dl.ox-review-facts
    div
      dt Expected disk revision
      dd
        code {{ shortRevision(diskRevision) }}
    div
      dt Candidate revision
      dd
        code {{ result ? shortRevision(result.candidateRevision) : '--' }}
    div
      dt Active now
      dd
        code {{ activeRevision ? shortRevision(activeRevision) : 'None' }}
    div
      dt Normalization
      dd {{ normalizationChanged ? 'Server normalized the submitted model' : 'No model changes' }}
  Banner.dialog-error(v-if="error" tone="bad" role="alert") {{ error }}
  Banner.review-warning(v-if="result?.restartRequired" tone="warn") {{ restartRequiredMessage }}
  Banner.review-warning(v-else tone="info") A changed canonical file is queued for in-process activation; no process restart is required.
  ConfigDiff(:lines="diff")
  Actions
    Button(autofocus data-close :disabled="saving") Continue editing
    Button(variant="primary" :disabled="saving || !canSave" @click="emit('save')") {{ saving ? 'Saving...' : 'Save canonical configuration' }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import Banner from '#design/banner/Banner.vue'
import Button from '#design/button/Button.vue'
import Actions from '#design/dialog/Actions.vue'
import Dialog from '#design/dialog/Dialog.vue'
import type { ConfigValidationResponse } from '../../api'
import { shortRevision } from '../../formatters'
import ConfigDiff from '../diff/ConfigDiff.vue'
import type { DiffLine } from '../diff/lines'
import './review.css'

const props = defineProps<{
  open: boolean
  diskRevision: string
  activeRevision: string | null
  result: ConfigValidationResponse | null
  diff: DiffLine[]
  normalizationChanged: boolean
  error: string | null
  saving: boolean
  canSave: boolean
}>()
const emit = defineEmits<{ close: []; save: [] }>()

const restartRequiredMessage = computed(() =>
  props.result?.diagnostics.find(({ code }) => code === 'I_RESTART_REQUIRED')?.message ??
    'This configuration takes effect after the next process restart.',
)
</script>
