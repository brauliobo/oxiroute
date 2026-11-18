<script setup vapor lang="ts">
import { computed, ref, watch } from 'vue'
import Button from '#design/button/Button.vue'
import Actions from '#design/dialog/Actions.vue'
import Dialog from '#design/dialog/Dialog.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import { pendingConfirm, settleConfirm } from './confirm'
import './styles.css'

const typed = ref('')
const open = computed(() => pendingConfirm.value !== null)
const title = computed(() => pendingConfirm.value?.title ?? '')
const required = computed(() => pendingConfirm.value?.typeToConfirm)
const ready = computed(() => required.value === undefined || typed.value === required.value)

watch(pendingConfirm, () => {
  typed.value = ''
})
</script>

<template lang="pug">
Dialog(:open="open" :title="title" @close="settleConfirm(false)")
  template(v-if="pendingConfirm")
    p {{ pendingConfirm.message }}
    ul.ox-confirm-details(v-if="pendingConfirm.details?.length")
      li(v-for="detail in pendingConfirm.details" :key="detail") {{ detail }}
    Field.ox-confirm-typed(v-if="required" :label="`Type ${required} to confirm`" label-for="ox-confirm-typed")
      Input#ox-confirm-typed(v-model="typed" autocomplete="off" spellcheck="false")
    Actions
      Button(autofocus data-close) Cancel
      Button(
        :variant="pendingConfirm.danger ? 'danger' : 'primary'"
        :disabled="!ready"
        data-confirm
        @click="settleConfirm(true)"
      ) {{ pendingConfirm.confirmLabel }}
</template>
