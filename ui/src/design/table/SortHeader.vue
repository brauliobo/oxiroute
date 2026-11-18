<script setup vapor lang="ts">
import { computed } from 'vue'
import './styles.css'

const MARKS = { ascending: '▲', descending: '▼', none: '' }

const props = defineProps<{ label: string; sortable?: boolean; direction?: 'ascending' | 'descending' | 'none' }>()
const emit = defineEmits<{ sort: [] }>()
const mark = computed(() => MARKS[props.direction ?? 'none'])
</script>

<template lang="pug">
th(scope="col" :aria-sort="sortable ? direction ?? 'none' : undefined")
  button.ox-sort(v-if="sortable" type="button" @click="emit('sort')")
    | {{ label }}
    span.ox-sort-mark(aria-hidden="true") {{ mark }}
  template(v-else) {{ label }}
</template>
