<template lang="pug">
Card.loading-panel
  Loading(:label="title")
  p {{ detail }}
</template>

<script setup lang="ts" vapor>
import Loading from '#design/empty/Loading.vue'
import Card from '#design/panel/Card.vue'

defineProps<{ title: string; detail: string }>()
</script>
