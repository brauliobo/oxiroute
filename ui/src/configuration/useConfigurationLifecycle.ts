import { computed, ref, watch, type Ref } from 'vue'

import { ApiError, saveConfig, validateConfig } from '../api'
import type {
  CanonicalConfig,
  ConfigDiagnostic,
  ConfigSaveResponse,
  ConfigSnapshot,
  ConfigValidationResponse,
} from '../config'
import { errorDiagnosticsFrom } from '../config'
import { isRecord } from '../valueGuards'

export interface SaveMessage {
  kind: 'success' | 'pending' | 'error'
  title: string
  detail: string
}

interface ConfigWriteFailure {
  diskRevision: string | null
  activeRevision: string | null
  outcome: 'write_failed'
  diagnostics: ConfigDiagnostic[]
}

interface ConfigurationLifecycleOptions {
  draft: Ref<CanonicalConfig | null>
  snapshot: Ref<ConfigSnapshot | null>
  diskRevision: Ref<string>
  activeRevision: Ref<string | null>
  diskDiagnostics: Ref<ConfigDiagnostic[]>
  staleRevision: Ref<string | null>
  accessToken: Ref<string | null>
  onUnauthorized: () => void
  onExitReview: (fallbackSelector?: string) => void
}

export function useConfigurationLifecycle(options: ConfigurationLifecycleOptions) {
  const validating = ref(false)
  const saving = ref(false)
  const validationResult = ref<ConfigValidationResponse | null>(null)
  const validatedFingerprint = ref<string | null>(null)
  const saveMessage = ref<SaveMessage | null>(null)
  const dialogError = ref<string | null>(null)
  const attemptDiagnostics = ref<ConfigDiagnostic[] | null>(null)
  let validationController: AbortController | null = null
  let saveController: AbortController | null = null
  let validationGeneration = 0

  const draftFingerprint = computed(() =>
    options.draft.value ? JSON.stringify(options.draft.value) : '',
  )
  const diskFingerprint = computed(() =>
    options.snapshot.value ? JSON.stringify(options.snapshot.value.config) : '',
  )
  const isDirty = computed(() => draftFingerprint.value !== diskFingerprint.value)
  const validationCurrent = computed(
    () => validatedFingerprint.value !== null && validatedFingerprint.value === draftFingerprint.value,
  )
  const visibleDiagnostics = computed(() =>
    attemptDiagnostics.value ?? (validationCurrent.value && validationResult.value
      ? validationResult.value.diagnostics
      : options.diskDiagnostics.value),
  )
  const diagnosticContext = computed(() => {
    if (validationCurrent.value) return 'candidate'
    return attemptDiagnostics.value === null ? 'disk snapshot' : 'rejected candidate'
  })
  const canReviewSave = computed(
    () =>
      validationCurrent.value &&
      validationResult.value !== null &&
      options.snapshot.value?.compositional === false &&
      !validationResult.value.compositional &&
      !validationResult.value.diagnostics.some((diagnostic) => diagnostic.severity === 'error') &&
      options.staleRevision.value === null,
  )
  const normalizationChanged = computed(
    () =>
      validationResult.value !== null &&
      JSON.stringify(validationResult.value.normalizedConfig) !== draftFingerprint.value,
  )

  watch(draftFingerprint, (fingerprint, previousFingerprint) => {
    if (fingerprint === previousFingerprint) return
    validationGeneration += 1
    validationController?.abort()
    validationController = null
    validating.value = false
    validatedFingerprint.value = null
    validationResult.value = null
  })

  function clearMessages(): void {
    saveMessage.value = null
    attemptDiagnostics.value = null
  }

  function resetForSnapshot(): void {
    validationResult.value = null
    validatedFingerprint.value = null
    saveMessage.value = null
    attemptDiagnostics.value = null
  }

  async function runValidation(): Promise<void> {
    const token = options.accessToken.value
    if (!options.draft.value || !token) return
    validationController?.abort()
    const controller = new AbortController()
    validationController = controller
    const generation = ++validationGeneration
    validating.value = true
    validationResult.value = null
    validatedFingerprint.value = null
    saveMessage.value = null
    attemptDiagnostics.value = null
    const submittedFingerprint = draftFingerprint.value
    try {
      const result = await validateConfig(clone(options.draft.value), token, controller.signal)
      if (!validationRequestCurrent(generation, submittedFingerprint, controller)) return
      validationResult.value = result
      validatedFingerprint.value = submittedFingerprint
    } catch (error) {
      if (!validationRequestCurrent(generation, submittedFingerprint, controller)) return
      if (error instanceof ApiError && error.status === 401) {
        options.onUnauthorized()
        return
      }
      if (error instanceof ApiError && error.status === 422) {
        attemptDiagnostics.value = errorDiagnosticsFrom(error.payload)
        saveMessage.value = {
          kind: 'error',
          title: 'Candidate is invalid.',
          detail: 'Nothing was written; correct the blocking diagnostics and validate again.',
        }
      } else {
        saveMessage.value = {
          kind: 'error',
          title: 'Validation unavailable.',
          detail: error instanceof Error ? error.message : 'The validation request failed.',
        }
      }
    } finally {
      if (validationController === controller) validationController = null
      if (generation === validationGeneration) validating.value = false
    }
  }

  async function writeCandidate(): Promise<void> {
    const token = options.accessToken.value
    if (!canReviewSave.value || !validationResult.value || !token) return
    const validation = validationResult.value
    saveController?.abort()
    const controller = new AbortController()
    saveController = controller
    saving.value = true
    dialogError.value = null
    const normalized = clone(validationResult.value.normalizedConfig)
    try {
      const result = await saveConfig(
        normalized,
        options.diskRevision.value,
        token,
        controller.signal,
      )
      if (controller.signal.aborted) return
      applySaveResult(result, normalized, validation)
      options.onExitReview('.revision-banner.save-state')
    } catch (error) {
      if (controller.signal.aborted) return
      if (error instanceof ApiError && error.status === 401) {
        options.onUnauthorized()
      } else if (error instanceof ApiError && error.status === 428) {
        options.staleRevision.value = 'unknown'
        dialogError.value = 'The server rejected the configuration revision precondition. Reload the authoritative revision before retrying.'
      } else if (error instanceof ApiError && error.status === 409) {
        const payload = revisionPayload(error.payload)
        options.staleRevision.value = payload?.diskRevision ?? 'newer revision'
        if (payload?.activeRevision !== undefined) options.activeRevision.value = payload.activeRevision
        attemptDiagnostics.value = errorDiagnosticsFrom(error.payload)
        saveMessage.value = {
          kind: 'error',
          title: 'Save conflict; draft preserved.',
          detail: 'The If-Config-Revision value is stale. No write occurred.',
        }
        options.onExitReview('.revision-banner.stale')
      } else if (error instanceof ApiError && error.status === 422) {
        validatedFingerprint.value = null
        validationResult.value = null
        attemptDiagnostics.value = errorDiagnosticsFrom(error.payload)
        saveMessage.value = {
          kind: 'error',
          title: 'Save rejected as invalid.',
          detail: 'No write occurred; review the validation diagnostics.',
        }
        options.onExitReview('#diagnostics-heading')
      } else if (error instanceof ApiError && error.status === 503 &&
        error.code === 'authoritative_config_unavailable'
      ) {
        options.staleRevision.value = 'unknown'
        attemptDiagnostics.value = errorDiagnosticsFrom(error.payload)
        saveMessage.value = {
          kind: 'error',
          title: 'Authoritative configuration unavailable; draft preserved.',
          detail: 'The server could not reload the latest disk state after a conflict. Reload before retrying.',
        }
        options.onExitReview('.revision-banner.stale')
      } else if (error instanceof ApiError && error.status === 500) {
        applyWriteFailure(error.payload)
      } else {
        dialogError.value = error instanceof Error ? error.message : 'The write request failed.'
      }
    } finally {
      if (saveController === controller) saveController = null
      saving.value = false
    }
  }

  function applySaveResult(
    result: ConfigSaveResponse,
    config: CanonicalConfig,
    validation: ConfigValidationResponse,
  ): void {
    options.diskRevision.value = result.diskRevision
    options.activeRevision.value = result.activeRevision
    options.diskDiagnostics.value = result.diagnostics
    options.snapshot.value = {
      schemaVersion: 1,
      diskRevision: result.diskRevision,
      candidateRevision: result.candidateRevision,
      activeRevision: result.activeRevision,
      config: clone(config),
      configFormat: validation.configFormat,
      compositional: validation.compositional,
      dependencyCount: validation.dependencyCount,
      configPreview: validation.configPreview,
      ...(validation.luaPreview === undefined ? {} : { luaPreview: validation.luaPreview }),
      diagnostics: result.diagnostics,
    }
    options.draft.value = clone(config)
    validationResult.value = null
    validatedFingerprint.value = null
    options.staleRevision.value = null
    attemptDiagnostics.value = null
    saveMessage.value = result.outcome === 'unchanged_active'
      ? {
          kind: 'success',
          title: 'Configuration unchanged; active generation retained.',
          detail: 'No restart is required because the canonical generation did not change.',
        }
      : {
          kind: 'pending',
          title: 'Configuration saved; restart required.',
          detail: 'The canonical file changed. Restart OxiRoute to activate the saved generation.',
        }
  }

  function applyWriteFailure(value: unknown): void {
    const failure = writeFailurePayload(value)
    if (failure) {
      options.diskDiagnostics.value = failure.diagnostics
      attemptDiagnostics.value = failure.diagnostics
      options.activeRevision.value = failure.activeRevision
      if (failure.diskRevision === null || failure.diskRevision !== options.diskRevision.value) {
        options.staleRevision.value = failure.diskRevision ?? 'unknown'
      }
      dialogError.value = 'The canonical write failed. Disk state is uncertain; reload before retrying.'
    } else {
      options.staleRevision.value = 'unknown'
      dialogError.value = 'The save failed without an authoritative revision payload. Reload before retrying.'
    }
  }

  function abortRequests(): void {
    validationGeneration += 1
    validationController?.abort()
    saveController?.abort()
    validationController = null
    saveController = null
    validating.value = false
    saving.value = false
  }

  function validationRequestCurrent(
    generation: number,
    fingerprint: string,
    controller: AbortController,
  ): boolean {
    return !controller.signal.aborted &&
      generation === validationGeneration &&
      draftFingerprint.value === fingerprint
  }

  return {
    attemptDiagnostics,
    canReviewSave,
    diagnosticContext,
    dialogError,
    isDirty,
    normalizationChanged,
    saveMessage,
    saving,
    validating,
    validationCurrent,
    validationResult,
    visibleDiagnostics,
    abortRequests,
    clearMessages,
    resetForSnapshot,
    runValidation,
    writeCandidate,
  }
}

function writeFailurePayload(value: unknown): ConfigWriteFailure | null {
  if (!isRecord(value) ||
    value.outcome !== 'write_failed' ||
    (typeof value.diskRevision !== 'string' && value.diskRevision !== null) ||
    (typeof value.activeRevision !== 'string' && value.activeRevision !== null) ||
    !Array.isArray(value.diagnostics)
  ) return null
  return value as unknown as ConfigWriteFailure
}

function revisionPayload(
  value: unknown,
): { diskRevision: string; activeRevision?: string | null } | null {
  return isRecord(value) && typeof value.diskRevision === 'string'
    ? (value as { diskRevision: string; activeRevision?: string | null })
    : null
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}
