<script setup vapor lang="ts">
import Checkbox from '#design/field/Checkbox.vue'
import './page.css'

defineProps<{ force: boolean }>()
const emit = defineEmits<{ 'update:force': [force: boolean] }>()
</script>

<template lang="pug">
Checkbox(:model-value="force" @update:modelValue="(next) => emit('update:force', next)") Force evacuation: also cut requests that cannot move
</template>
