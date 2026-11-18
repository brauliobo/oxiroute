<script setup vapor lang="ts">
import { ref } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Actions from '#design/dialog/Actions.vue'
import Dialog from '#design/dialog/Dialog.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Toast from '#design/toast/Toast.vue'

const open = ref(false)
</script>

<template lang="pug">
section.ox-demo-section(aria-labelledby="demo-feedback")
  h2#demo-feedback Banners, notices, states, toasts and dialogs
  Banner(tone="ok") The configuration was saved.
  Banner(tone="info") A candidate revision is waiting.
  Banner(tone="warn")
    | The data is stale.
    template(v-slot:actions)
      Button Retry now
  Banner(tone="bad") The server cannot be reached.
  Notice(kind="status")
    p Pool web is now ready.
  .ox-demo-grid
    EmptyState(title="No pools") No pools are configured.
    Loading(label="Loading certificates")
  .ox-demo-grid
    Toast(tone="ok" text="Saved")
    Toast(tone="bad" text="Could not save")
  .ox-demo-row
    Button(@click="open = true") Open dialog
  Dialog(:open="open" title="Confirm administration" @close="open = false")
    p Set server origin-a of pool web to drain?
    Actions
      Button(autofocus data-close) Cancel
      Button(variant="primary" data-close) Confirm
</template>
