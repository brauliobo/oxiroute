<template lang="pug">
.capability-panel
  span.capability-index {{ code }}
  div
    h3 {{ title }}
    p {{ message }}
    p {{ note }}
    button.secondary-button(type="button" @click="emit('retry')") {{ retryLabel }}
</template>

<script setup lang="ts" vapor>
defineProps<{ code: string; title: string; message: string; note: string; retryLabel: string }>()
const emit = defineEmits<{ retry: [] }>()
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./panels.css"></style>
