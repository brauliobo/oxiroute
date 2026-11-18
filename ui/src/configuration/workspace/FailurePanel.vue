<template lang="pug">
Card.capability-panel
  span.capability-index {{ code }}
  div
    h3 {{ title }}
    p {{ message }}
    p {{ note }}
    Button.secondary-button(@click="emit('retry')") {{ retryLabel }}
</template>

<script setup lang="ts" vapor>
import Card from '#design/panel/Card.vue'
import Button from '#design/button/Button.vue'
defineProps<{ code: string; title: string; message: string; note: string; retryLabel: string }>()
const emit = defineEmits<{ retry: [] }>()
</script>
