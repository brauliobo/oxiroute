<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import Card from '#design/panel/Card.vue'
import type { TlsCertificateInventory } from '../api'
import Facts from '../workspace/Facts.vue'
import { CERTIFICATE_ACTIONS } from './actions'
import type { CertificateAction } from './actions'
import { expiryBadge } from './expiry'
import { certificateActions, certificateFacts, expiresAtMs, sourceLabel, statusBadge } from './model'
import './styles.css'

const props = defineProps<{ certificate: TlsCertificateInventory; nowMs: number; disabled: boolean }>()
const emit = defineEmits<{ act: [action: CertificateAction] }>()

const status = computed(() => statusBadge(props.certificate))
const expiry = computed(() => props.certificate.status && expiryBadge(expiresAtMs(props.certificate.status), props.nowMs))
const facts = computed(() => certificateFacts(props.certificate, props.nowMs))
const actions = computed(() => certificateActions(props.certificate).map((id) => ({ id, ...CERTIFICATE_ACTIONS[id] })))
</script>

<template lang="pug">
Card.ox-cert
  header.ox-cert-head
    div
      h3 {{ certificate.name }}
      p.ox-muted {{ sourceLabel(certificate) }}
    .ox-cert-badges
      Badge(:tone="status.tone" :label="status.label")
      Badge(v-if="expiry" :tone="expiry.tone" :label="expiry.label")
  Facts(:items="facts")
  .ox-row-actions(v-if="actions.length")
    Button(v-for="action in actions" :key="action.id" :variant="action.variant" :disabled="disabled" @click="emit('act', action.id)") {{ action.label }}
</template>
