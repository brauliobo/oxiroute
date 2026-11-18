<script setup vapor lang="ts">
import Toast from './Toast.vue'
import { dismissToast, toasts } from './state'
import './styles.css'
</script>

<template lang="pug">
section.ox-toasts(aria-label="Notifications")
  Toast(v-for="toast in toasts" :key="toast.id" :tone="toast.tone" :text="toast.text" @dismiss="dismissToast(toast.id)")
</template>
