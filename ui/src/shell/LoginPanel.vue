<script setup vapor lang="ts">
import { ref } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Panel from '#design/panel/Panel.vue'
import './styles.css'

defineProps<{ rejected: boolean }>()
const emit = defineEmits<{ submit: [token: string] }>()

const input = ref('')

function submit(): void {
  if (!input.value) return
  emit('submit', input.value)
  input.value = ''
}
</script>

<template lang="pug">
Panel.ox-login(title="Management token")
  Notice(v-if="rejected" kind="alert")
    Banner(tone="bad")
      strong Management token rejected.
      |  The server did not accept it. Enter a valid bearer token.
  Notice(v-else kind="status")
    Banner(tone="info")
      strong No management token.
      |  Enter the management bearer token to open the console.
  form.ox-login-form(@submit.prevent="submit")
    Field(label="Bearer token" label-for="management-access-token")
      Input#management-access-token(
        v-model="input"
        type="password"
        autocomplete="off"
        spellcheck="false"
        aria-describedby="management-token-note"
      )
    p#management-token-note.ox-muted The token is kept for this browser tab only. It is never stored on disk or placed in the URL, and signing out clears it.
    Button(variant="primary" type="submit" :disabled="input.length === 0") Unlock console
</template>
