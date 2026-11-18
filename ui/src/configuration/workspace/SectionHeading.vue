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
