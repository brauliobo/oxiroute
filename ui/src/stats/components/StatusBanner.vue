<script setup vapor lang="ts">
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import type { Banner as BannerModel } from '../banner'

defineProps<{ banner: BannerModel | undefined }>()
const emit = defineEmits<{ action: [action: 'retry' | 'reload'] }>()
const LABELS = { retry: 'Retry now', reload: 'Reload page' }
</script>

<template lang="pug">
Notice(kind="alert")
  Banner(v-if="banner" :tone="banner.tone")
    | {{ banner.text }}
    template(v-slot:actions)
      Button(v-if="banner.action" @click="emit('action', banner.action)") {{ LABELS[banner.action] }}
</template>
