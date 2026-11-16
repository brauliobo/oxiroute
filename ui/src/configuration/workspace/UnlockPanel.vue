<template lang="pug">
form.unlock-panel(
  data-unlock-form
  aria-labelledby="config-unlock-heading"
  @submit.prevent="unlock"
)
  span.capability-index(aria-hidden="true") AUTH
  div
    h3#config-unlock-heading Unlock canonical configuration
    p Enter the management API bearer token. It is held only in memory for this page session.
    label.unlock-label(for="config-access-token") Bearer token
    input#config-access-token(
      v-model="tokenInput"
      type="password"
      autocomplete="off"
      required
      aria-describedby="config-token-note"
    )
    p#config-token-note The token is never placed in browser storage or the URL.
    p.unlock-error(v-if="error" role="alert") {{ error }}
    button.primary-button(type="submit" :disabled="unlocking || tokenInput.length === 0") {{ unlocking ? 'Unlocking...' : 'Unlock configuration' }}
</template>

<script setup lang="ts" vapor>
import { ref } from 'vue'

defineProps<{ unlocking: boolean; error: string | null }>()
const emit = defineEmits<{ unlock: [token: string] }>()

const tokenInput = ref('')

function unlock(): void {
  const token = tokenInput.value
  if (!token) return
  tokenInput.value = ''
  emit('unlock', token)
}
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./panels.css"></style>
<style scoped>
.unlock-panel {
  border-color: #596b4a;
}

.unlock-panel > div {
  width: min(520px, 100%);
}

.unlock-label {
  display: block;
  margin: 20px 0 7px;
  color: #cbd2c4;
  font-size: 0.78rem;
  font-weight: 700;
}

.unlock-panel input {
  width: 100%;
  min-height: 44px;
  padding: 9px 11px;
  border: 1px solid #59634f;
  color: #eef2e7;
  background: #0d100c;
  font: 0.82rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.unlock-panel input:focus-visible {
  outline: 2px solid #ffffff;
  outline-offset: 2px;
}
</style>
