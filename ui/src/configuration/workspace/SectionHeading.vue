<template lang="pug">
header.form-heading
  div
    p.eyebrow {{ eyebrow }}
    h3 {{ title }}
  span.object-path {{ path }}
</template>

<script setup lang="ts" vapor>
defineProps<{ eyebrow: string; title: string; path: string }>()
</script>

<style scoped src="./base.css"></style>
<style scoped>
.form-heading {
  align-items: flex-start;
  margin-bottom: 28px;
  padding-bottom: 18px;
  border-bottom: 1px solid #34392f;
}

.object-path {
  text-transform: none;
}
</style>
