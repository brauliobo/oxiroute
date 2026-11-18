<script setup vapor lang="ts">
import { computed } from 'vue'
import type { ButtonVariant } from './types'
import './styles.css'

const props = withDefaults(defineProps<{
  variant?: ButtonVariant
  small?: boolean
  pressed?: boolean
  type?: 'button' | 'submit'
}>(), { variant: 'default', type: 'button', pressed: undefined })

const classes = computed(() => [props.variant !== 'default' && `ox-${props.variant}`, props.small && 'ox-small'])
</script>

<template lang="pug">
button.ox-btn(:type="type" :class="classes" :aria-pressed="pressed")
  slot
</template>
