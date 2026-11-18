<template lang="pug">
article
  slot
</template>

<script setup lang="ts" vapor>
import { provideItemScope } from './context'

const props = defineProps<{ index: number }>()
provideItemScope(() => props.index)
</script>
