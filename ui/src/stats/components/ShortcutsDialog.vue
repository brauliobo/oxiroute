<script setup vapor lang="ts">
import Button from '#design/button/Button.vue'
import Actions from '#design/dialog/Actions.vue'
import Dialog from '#design/dialog/Dialog.vue'
import Kbd from '#design/kbd/Kbd.vue'
import { SHORTCUTS } from '../shortcuts'

defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()
</script>

<template lang="pug">
Dialog(:open="open" title="Keyboard shortcuts" @close="emit('close')")
  dl.ox-shortcuts
    .ox-shortcut(v-for="shortcut in SHORTCUTS" :key="shortcut.keys")
      dt
        Kbd {{ shortcut.keys }}
      dd {{ shortcut.description }}
  Actions
    Button(autofocus data-close) Close
</template>
