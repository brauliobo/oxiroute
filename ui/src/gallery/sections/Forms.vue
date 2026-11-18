<script setup vapor lang="ts">
import { ref } from 'vue'
import Checkbox from '#design/field/Checkbox.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'

const search = ref('web')
const pool = ref('web')
const problems = ref(false)
const pools = [{ value: 'web', label: 'web' }, { value: 'api', label: 'api' }, { value: 'static', label: 'static' }]
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-forms")
  h2#demo-forms Fields
  .ox-demo-row
    Field(label="Search" label-for="demo-search")
      Input#demo-search(v-model="search" type="search" placeholder="Search pools")
    Field(label="Pool" label-for="demo-pool")
      Select#demo-pool(v-model="pool" :options="pools")
    Field(label="Disabled" label-for="demo-disabled")
      Input#demo-disabled(model-value="read only" disabled)
    Checkbox(v-model="problems") Problems only
    Checkbox(:model-value="true" disabled) Locked
</template>
