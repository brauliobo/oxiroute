import { computed, ref, shallowRef } from 'vue'
import { pushToast } from '#design/toast/state'
import { ApiError } from '../api'
import { presentApiError } from '../formatters'
import { confirmAction } from '../shell/confirm'
import type { ConfirmRequest } from '../shell/confirm'

const TOAST_MS = 6000

export type OutcomeTone = 'ok' | 'warn' | 'bad'

export interface ActionOutcome<Scope extends string = string> {
  scope: Scope
  tone: OutcomeTone
  summary: string
  details: readonly string[]
}

export interface Success {
  tone: 'ok' | 'warn'
  summary: string
  details?: readonly string[]
}

export interface ActionPlan<Scope extends string, Context, Result> {
  scope: Scope
  failure: string
  confirm?: ConfirmRequest
  reloadOnSuccess?: boolean
  run: (context: Context, token: string) => Promise<Result>
  succeeded: (result: Result) => Success
}

interface RunnerOptions<Context> {
  token: () => string
  context: () => Context | null
  conflictHint: string
  reload: () => Promise<void>
  onUnauthorized: () => void
}

const isStatus = (error: unknown, status: number) => error instanceof ApiError && error.status === status

export function useActionRunner<Scope extends string, Context>(options: RunnerOptions<Context>) {
  const outcome = shallowRef<ActionOutcome<Scope> | null>(null)
  const running = ref(false)
  const canAct = computed(() => options.token() !== '' && options.context() !== null)

  function publish(next: ActionOutcome<Scope>): void {
    outcome.value = next
    pushToast(next.summary, next.tone, TOAST_MS)
  }

  function failureOutcome(scope: Scope, failure: string, error: unknown): ActionOutcome<Scope> {
    const reason = presentApiError(error, 'The management API did not respond.').replace(/\.$/, '')
    return {
      scope,
      tone: 'bad',
      summary: `Could not ${failure}: ${reason}.`,
      details: isStatus(error, 409) ? [options.conflictHint] : [],
    }
  }

  async function execute<Result>(plan: ActionPlan<Scope, Context, Result>, context: Context): Promise<boolean> {
    try {
      const { tone, summary, details = [] } = plan.succeeded(await plan.run(context, options.token()))
      publish({ scope: plan.scope, tone, summary, details })
      return plan.reloadOnSuccess !== false
    } catch (error) {
      if (isStatus(error, 401)) options.onUnauthorized()
      publish(failureOutcome(plan.scope, plan.failure, error))
      return isStatus(error, 409)
    }
  }

  async function perform<Result>(plan: ActionPlan<Scope, Context, Result>): Promise<void> {
    const context = options.context()
    if (running.value || options.token() === '' || context === null) return
    if (plan.confirm && !(await confirmAction(plan.confirm))) return
    running.value = true
    try {
      if (await execute(plan, context)) await options.reload()
    } finally {
      running.value = false
    }
  }

  const dismiss = (): void => {
    outcome.value = null
  }

  return { outcome, running, canAct, perform, dismiss }
}
