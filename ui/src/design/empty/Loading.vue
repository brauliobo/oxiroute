<script setup vapor lang="ts">
import './styles.css'

withDefaults(defineProps<{ label?: string }>(), { label: 'Loading' })
</script>

<template lang="pug">
.ox-loading(role="status")
  span.ox-spinner(aria-hidden="true")
  span {{ label }}
</template>
