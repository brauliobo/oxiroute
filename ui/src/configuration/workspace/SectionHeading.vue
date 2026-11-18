<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow {{ eyebrow }}
    h3 {{ title }}
  span.cfg-object-path {{ path }}
</template>

<script setup lang="ts" vapor>
defineProps<{ eyebrow: string; title: string; path: string }>()
</script>
