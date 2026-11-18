<script setup vapor lang="ts">
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'

defineProps<{ paused: boolean }>()
const emit = defineEmits<{ toggle: []; refresh: [] }>()
</script>

<template lang="pug">
Field
  Button(@click="emit('toggle')") {{ paused ? 'Resume' : 'Pause' }}
  Button(@click="emit('refresh')") Refresh now
</template>
