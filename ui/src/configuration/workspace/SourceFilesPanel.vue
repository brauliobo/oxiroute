<template lang="pug">
.source-workspace
  .source-rail
    .source-rail-head
      p.nav-section-label Workspace files
      Button(small :disabled="loadingList" @click="loadFiles") {{ loadingList ? 'Refreshing...' : 'Refresh' }}
    p.nav-empty(v-if="files.length === 0 && !loadingList") No configuration files discovered.
    .source-file-list(role="navigation" aria-label="Configuration files")
      Button.object-link(
        v-for="entry in files"
        :key="entry.path"
        :aria-current="!creating && selectedPath === entry.path ? 'page' : undefined"
        @click="openFile(entry.path)"
      )
        span {{ entry.path }}
        small {{ entry.kind === 'root' ? 'Root' : 'Include' }} · {{ entry.format }} · {{ entry.sizeBytes }} B
    .source-new-file
      p.nav-section-label Add include file
      Input(v-model="newFilePath" placeholder="includes/pools.oxi")
      Button(small :disabled="!configWritable || !newFilePath.trim()" @click="startCreate") + New include file

  .source-editor-pane
    Notice(kind="alert")
      Banner(v-if="error" tone="bad")
        strong Source operation failed.
        |  {{ error }}
    Notice(kind="status")
      Banner(v-if="saveNotice" tone="ok")
        strong {{ saveNotice }}
        template(#actions)
          Button(v-if="activeRevision" small variant="primary" :disabled="reloading" @click="triggerReload") {{ reloading ? 'Reloading...' : 'Reload generation now' }}
    Notice(kind="status")
      Banner(v-if="withheld" tone="warn")
        strong File text withheld for safety.
        |  One of this file's inline secrets cannot be safely redacted over HTTP. Edit it on disk or replace the secret with a secret file.

    header.source-editor-head
      div
        h3 {{ creating ? `New file: ${selectedPath}` : (selectedPath || 'Select a file') }}
        p.ox-muted(v-if="selectedFile")
          | Format: {{ selectedFile.format ?? selectedFile.kind }} · Revision: {{ shortRevision(selectedFile.fileRevision) }}
          template(v-if="selectedFile.redaction.redacted > 0")
            |  · {{ selectedFile.redaction.redacted }} secret(s) redacted (preserved on save)
      .source-editor-actions
        Button(:disabled="busy || !selectedPath || withheld" @click="runValidate") {{ validating ? 'Validating...' : 'Validate graph' }}
        Button(variant="primary" :disabled="busy || !configWritable || !selectedPath || withheld || !dirty" @click="runSave") {{ saving ? 'Saving...' : (creating ? 'Create file' : 'Save file') }}

    Textarea(
      v-if="selectedPath && !withheld"
      v-model="editorText"
      :rows="22"
      aria-label="Configuration file source"
    )

    .source-convert-bar(v-if="selectedPath")
      Field(label="Convert preview to format")
        Select(v-model="convertTarget" :options="formatOptions")
      Button(small :disabled="converting" @click="runConvert") {{ converting ? 'Converting...' : 'Render converted preview' }}

    Panel.cfg-preview-panel(v-if="convertedPreview !== null" :title="`Converted ${formatLabel(convertTarget)} preview`")
      pre {{ convertedPreview }}

    Panel.cfg-preview-panel(v-if="validation" title="Full-graph validation result")
      p.ox-muted
        | Candidate revision {{ shortRevision(validation.candidateRevision) }} · {{ validation.compositional ? `Compositional (${validation.dependencyCount} dependencies)` : 'Flat configuration' }}
        template(v-if="validation.restartRequired")
          |  · Restart required ({{ validation.restartReasons.length }})
      ul.diagnostic-list(v-if="validation.diagnostics.length > 0")
        li.diagnostic(v-for="(diag, idx) in validation.diagnostics" :key="idx" :class="`severity-${diag.severity}`")
          .diagnostic-code
            code {{ diag.code }}
            span {{ diag.stage }}
          div
            strong {{ diag.message }}
            p(v-if="diag.line") Line {{ diag.line }}
            p(v-if="diag.detail") {{ diag.detail }}
      pre(v-if="validation.textDiff") {{ validation.textDiff }}
</template>

<script setup lang="ts" vapor>
import { computed, onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Textarea from '#design/field/Textarea.vue'
import Panel from '#design/panel/Panel.vue'
import {
  ApiError,
  convertConfig,
  fetchConfigFile,
  fetchConfigFiles,
  reloadGeneration,
  saveConfigFile,
  validateConfigFile,
  type WorkspaceFileEntry,
  type WorkspaceFileReadResponse,
  type WorkspaceFileValidateResponse,
} from '../../api'
import { CONFIG_FORMATS, formatLabel, type ConfigFormat } from '../../config'
import { shortRevision } from '../../formatters'

const props = defineProps<{ token: string; activeRevision: string | null }>()
const emit = defineEmits<{ saved: []; unauthorized: [] }>()

const files = ref<WorkspaceFileEntry[]>([])
const configWritable = ref(true)
const loadingList = ref(false)
const loadingFile = ref(false)
const validating = ref(false)
const saving = ref(false)
const converting = ref(false)
const reloading = ref(false)
const selectedPath = ref('')
const selectedFile = ref<WorkspaceFileReadResponse | null>(null)
const creating = ref(false)
const newFilePath = ref('')
const editorText = ref('')
const originalText = ref('')
const error = ref<string | null>(null)
const saveNotice = ref<string | null>(null)
const validation = ref<WorkspaceFileValidateResponse | null>(null)
const convertTarget = ref<ConfigFormat>('oxi')
const convertedPreview = ref<string | null>(null)

const formatOptions = CONFIG_FORMATS.map((fmt) => ({ value: fmt, label: formatLabel(fmt) }))
const busy = computed(() => loadingList.value || loadingFile.value || validating.value || saving.value)
const withheld = computed(() => !creating.value && (selectedFile.value?.redaction.withheld ?? false))
const dirty = computed(() => creating.value || editorText.value !== originalText.value)

function handleError(err: unknown): void {
  if (err instanceof ApiError && err.status === 401) {
    emit('unauthorized')
    return
  }
  error.value = err instanceof Error ? err.message : 'Unexpected configuration error'
}

async function loadFiles(): Promise<void> {
  if (!props.token) return
  loadingList.value = true
  error.value = null
  try {
    const response = await fetchConfigFiles(props.token)
    files.value = response.files
    configWritable.value = response.configWritable
    if (!creating.value) {
      const target = selectedPath.value && response.files.some((entry) => entry.path === selectedPath.value)
        ? selectedPath.value
        : response.files.find((entry) => entry.kind === 'root')?.path ?? response.files[0]?.path ?? ''
      if (target) await openFile(target)
    }
  } catch (err) {
    handleError(err)
  } finally {
    loadingList.value = false
  }
}

async function openFile(relPath: string): Promise<void> {
  if (!props.token) return
  loadingFile.value = true
  creating.value = false
  selectedPath.value = relPath
  error.value = null
  saveNotice.value = null
  validation.value = null
  convertedPreview.value = null
  try {
    const file = await fetchConfigFile(relPath, props.token)
    selectedFile.value = file
    configWritable.value = file.configWritable
    editorText.value = file.text ?? ''
    originalText.value = file.text ?? ''
  } catch (err) {
    handleError(err)
  } finally {
    loadingFile.value = false
  }
}

function startCreate(): void {
  const rel = newFilePath.value.trim()
  if (!rel) return
  creating.value = true
  selectedPath.value = rel
  selectedFile.value = null
  editorText.value = ''
  originalText.value = ''
  validation.value = null
  error.value = null
  saveNotice.value = null
}

async function runValidate(): Promise<void> {
  if (!props.token || !selectedPath.value) return
  validating.value = true
  error.value = null
  try {
    validation.value = await validateConfigFile(selectedPath.value, editorText.value, props.token)
  } catch (err) {
    handleError(err)
  } finally {
    validating.value = false
  }
}

async function runSave(): Promise<void> {
  if (!props.token || !selectedPath.value) return
  saving.value = true
  error.value = null
  saveNotice.value = null
  try {
    const res = await saveConfigFile(
      selectedPath.value,
      editorText.value,
      selectedFile.value?.fileRevision ?? null,
      creating.value,
      props.token,
    )
    const wasCreating = creating.value
    creating.value = false
    newFilePath.value = ''
    saveNotice.value = `${wasCreating ? 'Created' : 'Saved'} ${selectedPath.value} (disk revision ${shortRevision(res.diskRevision)}).`
    await loadFiles()
    emit('saved')
  } catch (err) {
    handleError(err)
  } finally {
    saving.value = false
  }
}

async function runConvert(): Promise<void> {
  if (!props.token) return
  converting.value = true
  error.value = null
  try {
    const isRoot = selectedFile.value?.isRoot ?? false
    const response = await convertConfig(
      convertTarget.value,
      props.token,
      isRoot ? { text: editorText.value } : { view: 'disk' },
    )
    convertedPreview.value = response.text ?? 'Output withheld due to unredactable secret.'
  } catch (err) {
    handleError(err)
  } finally {
    converting.value = false
  }
}

async function triggerReload(): Promise<void> {
  if (!props.token || !props.activeRevision) return
  reloading.value = true
  error.value = null
  try {
    await reloadGeneration(props.activeRevision, props.token)
    saveNotice.value = 'Reload requested for the saved configuration.'
    emit('saved')
  } catch (err) {
    handleError(err)
  } finally {
    reloading.value = false
  }
}

watch(() => props.token, (token) => {
  if (token) void loadFiles()
})

onMounted(() => {
  if (props.token) void loadFiles()
})
</script>
