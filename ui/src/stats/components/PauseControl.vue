<script setup vapor lang="ts">
defineProps<{ paused: boolean }>()
const emit = defineEmits<{ toggle: []; refresh: [] }>()
</script>

<template lang="pug">
.ox-field
  button.ox-btn(type="button" @click="emit('toggle')") {{ paused ? 'Resume' : 'Pause' }}
  button.ox-btn(type="button" @click="emit('refresh')") Refresh now
</template>
