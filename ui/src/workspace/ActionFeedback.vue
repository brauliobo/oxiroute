<script setup vapor lang="ts">
import { computed, useTemplateRef } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import type { ActionOutcome } from './actions'
import './styles.css'

const props = defineProps<{ outcome: ActionOutcome | null; scope: string }>()
const emit = defineEmits<{ dismiss: [] }>()

const root = useTemplateRef<HTMLElement>('root')
const shown = computed(() => (props.outcome?.scope === props.scope ? props.outcome : null))
const failed = computed(() => shown.value?.tone === 'bad')
const succeeded = computed(() => shown.value !== null && !failed.value)

function dismiss(): void {
  emit('dismiss')
  root.value?.focus()
}
</script>

<template lang="pug">
.ox-feedback(ref="root" tabindex="-1" data-feedback)
  Notice(kind="status")
    template(v-if="shown && succeeded")
      Banner(:tone="shown.tone")
        strong {{ shown.summary }}
        template(v-slot:actions)
          Button(small @click="dismiss") Dismiss
      ul.ox-feedback-detail(v-if="shown.details.length")
        li(v-for="detail in shown.details" :key="detail") {{ detail }}
  Notice(kind="alert")
    template(v-if="shown && failed")
      Banner(tone="bad")
        strong {{ shown.summary }}
        template(v-slot:actions)
          Button(small @click="dismiss") Dismiss
      ul.ox-feedback-detail(v-if="shown.details.length")
        li(v-for="detail in shown.details" :key="detail") {{ detail }}
</template>
