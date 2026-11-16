<template lang="pug">
.review-scrim(role="presentation" @click.self="emit('close')")
  section.save-review(role="dialog" aria-modal="true" aria-labelledby="save-review-heading" @keydown.tab="trapFocus")
    header.review-heading
      div
        p.eyebrow Revision-checked write
        h3#save-review-heading Save review
      button.close-button(type="button" aria-label="Close save review" @click="emit('close')") Close
    dl.review-facts
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
    p.dialog-error(v-if="error" role="alert") {{ error }}
    p.review-warning(v-if="result?.restartRequired") {{ restartRequiredMessage }}
    p.review-warning(v-else) A changed canonical file is queued for in-process activation; no process restart is required.
    .review-actions
      button.secondary-button(type="button" @click="emit('close')") Continue editing
      button.primary-button(type="button" :disabled="saving || !canSave" @click="emit('save')") {{ saving ? 'Saving...' : 'Save canonical configuration' }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import type { ConfigValidationResponse } from '../../api'
import { shortRevision } from '../../formatters'

const props = defineProps<{
  diskRevision: string
  activeRevision: string | null
  result: ConfigValidationResponse | null
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

function trapFocus(event: KeyboardEvent): void {
  const dialog = event.currentTarget as HTMLElement
  const controls = Array.from(
    dialog.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'),
  )
  const first = controls[0]
  const last = controls.at(-1)
  if (!first || !last) return
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault()
    first.focus()
  }
}
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped>
.review-scrim {
  position: fixed;
  z-index: 20;
  display: grid;
  inset: 0;
  padding: 18px;
  overflow-y: auto;
  place-items: center;
  background: rgb(5 7 4 / 82%);
  backdrop-filter: blur(5px);
}

.save-review {
  width: min(620px, 100%);
  padding: clamp(22px, 5vw, 38px);
  border: 1px solid #657155;
  background: #151912;
  box-shadow: 0 30px 100px #000;
}

.review-heading {
  align-items: flex-start;
  padding-bottom: 18px;
  border-bottom: 1px solid #34392f;
}

.review-facts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 1px;
  margin: 20px 0;
  background: #34392f;
}

.review-facts div {
  display: grid;
  gap: 7px;
  padding: 14px;
  background: #0e110d;
}

.review-facts dd {
  margin: 0;
  color: #d5dbce;
  font-size: 0.78rem;
}

.review-warning {
  padding: 13px;
  border-left: 3px solid #ffbf4b;
  color: #bdc4b6;
  background: #1c1c15;
  font-size: 0.8rem;
  line-height: 1.55;
}

.dialog-error {
  margin: 18px 0 0;
  padding: 13px;
  border-left: 3px solid #ff745c;
  background: #241410;
  font-size: 0.8rem;
  line-height: 1.55;
}

.review-actions {
  justify-content: flex-end;
  margin-top: 24px;
}

@media (max-width: 700px) {
  .review-facts {
    grid-template-columns: 1fr;
  }

  .review-actions {
    align-items: stretch;
    flex-direction: column-reverse;
  }
}
</style>
