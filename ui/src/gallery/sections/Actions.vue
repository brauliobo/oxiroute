<script setup vapor lang="ts">
import { ref } from 'vue'
import Button from '#design/button/Button.vue'
import Chip from '#design/button/Chip.vue'
import Toggle from '#design/button/Toggle.vue'

const pressed = ref(true)
const expanded = ref(true)
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-actions")
  h2#demo-actions Buttons, chips and toggles
  .ox-demo-row
    Button Default
    Button(variant="primary") Primary
    Button(variant="danger") Danger
    Button(variant="link") Link
    Button(small) Small
    Button(disabled) Disabled
    Button(:pressed="pressed" @click="pressed = !pressed") Descending
  .ox-demo-row
    Chip(:pressed="true") Up
    Chip(:pressed="false") Down
    Chip(:pressed="false") Maintenance
  .ox-demo-row
    Toggle(:expanded="expanded" @click="expanded = !expanded") Pool web
</template>
