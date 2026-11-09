<script setup vapor lang="ts">
import { SHORTCUTS } from '../shortcuts'
import ModalDialog from './ModalDialog.vue'

defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()
</script>

<template lang="pug">
ModalDialog(:open="open" title="Keyboard shortcuts" @close="emit('close')")
  dl.ox-shortcuts
    .ox-shortcut(v-for="shortcut in SHORTCUTS" :key="shortcut.keys")
      dt
        kbd {{ shortcut.keys }}
      dd {{ shortcut.description }}
  .ox-dialog-actions
    button.ox-btn(type="button" autofocus data-close) Close
</template>
