<script setup vapor lang="ts">
import type { Theme } from '../composables/useTheme'
import ThemeToggle from './ThemeToggle.vue'
import './styles.css'

withDefaults(defineProps<{ title: string; skipLabel: string; mainId?: string; theme?: Theme }>(), { mainId: 'ox-main' })
const emit = defineEmits<{ 'update:theme': [theme: Theme] }>()
</script>

<template lang="pug">
.ox-shell
  a.ox-skip(:href="`#${mainId}`") {{ skipLabel }}
  header.ox-header
    h1 {{ title }}
    slot(name="header")
    ThemeToggle(v-if="theme" :model-value="theme" @update:modelValue="(next) => emit('update:theme', next)")
  slot(name="above")
  main(:id="mainId" tabindex="-1")
    slot
  slot(name="footer")
</template>
