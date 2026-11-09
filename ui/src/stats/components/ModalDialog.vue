<script setup vapor lang="ts">
import { onMounted, useId, useTemplateRef, watch } from 'vue'

// A native modal <dialog>: the browser traps focus, makes the page inert, closes on Escape and returns focus to the
// element that opened it. An element with `data-close`, or a click on the backdrop, closes it.
const props = defineProps<{ open: boolean; title: string }>()
const emit = defineEmits<{ close: [] }>()
const dialog = useTemplateRef<HTMLDialogElement>('dialog')
const titleId = useId()

function sync() {
  const element = dialog.value
  if (!element || props.open === element.open) return
  if (props.open) element.showModal()
  else element.close()
}
const onClick = (event: MouseEvent) => {
  const target = event.target as Element
  if (target === dialog.value || target.closest('[data-close]')) dialog.value?.close()
}

onMounted(sync)
watch(() => props.open, sync)
</script>

<template lang="pug">
dialog.ox-dialog(ref="dialog" :aria-labelledby="titleId" @click="onClick" @close="emit('close')")
  h2(:id="titleId") {{ title }}
  slot
</template>
