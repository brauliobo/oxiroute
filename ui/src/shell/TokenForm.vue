<script setup vapor lang="ts">
import { ref } from 'vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import './styles.css'

defineProps<{ signedIn: boolean }>()
const emit = defineEmits<{ submit: [token: string] }>()

const input = ref('')

function submit(): void {
  if (!input.value) return
  emit('submit', input.value)
  input.value = ''
}
</script>

<template lang="pug">
form.ox-token(@submit.prevent="submit")
  Field(label="Management token" label-for="management-access-token" hide-label)
    Input#management-access-token(
      v-model="input"
      type="password"
      autocomplete="off"
      spellcheck="false"
      placeholder="Bearer token"
    )
  Button(type="submit" :disabled="input.length === 0") {{ signedIn ? 'Replace token' : 'Unlock telemetry' }}
</template>
