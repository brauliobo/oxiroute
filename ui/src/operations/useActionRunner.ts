import { computed, ref, shallowRef } from 'vue'
import { pushToast } from '#design/toast/state'
import { ApiError } from '../api'
import { presentApiError } from '../formatters'
import { confirmAction } from '../shell/confirm'
import type { ActionOutcome, ActionPlan, Scope } from './types'

const TOAST_MS = 6000
const CONFLICT_HINT = 'The runtime changed since this page loaded; the current state was reloaded. Review it and retry.'

interface RunnerOptions {
  token: () => string
  revision: () => string | null
  reload: () => Promise<void>
  onUnauthorized: () => void
}

const isStatus = (error: unknown, status: number) => error instanceof ApiError && error.status === status

function failureOutcome(scope: Scope, failure: string, error: unknown): ActionOutcome {
  const reason = presentApiError(error, 'The management API did not respond.').replace(/\.$/, '')
  return {
    scope,
    tone: 'bad',
    summary: `Could not ${failure}: ${reason}.`,
    details: isStatus(error, 409) ? [CONFLICT_HINT] : [],
  }
}

export function useActionRunner(options: RunnerOptions) {
  const outcome = shallowRef<ActionOutcome | null>(null)
  const running = ref(false)
  const canAct = computed(() => options.token() !== '' && options.revision() !== null)

  function publish(next: ActionOutcome): void {
    outcome.value = next
    pushToast(next.summary, next.tone, TOAST_MS)
  }

  async function execute<Result>(plan: ActionPlan<Result>, revision: string): Promise<boolean> {
    try {
      const { tone, summary, details = [] } = plan.succeeded(await plan.run(revision, options.token()))
      publish({ scope: plan.scope, tone, summary, details })
      return true
    } catch (error) {
      if (isStatus(error, 401)) options.onUnauthorized()
      publish(failureOutcome(plan.scope, plan.failure, error))
      return isStatus(error, 409)
    }
  }

  async function perform<Result>(plan: ActionPlan<Result>): Promise<void> {
    const revision = options.revision()
    if (running.value || options.token() === '' || revision === null) return
    if (plan.confirm && !(await confirmAction(plan.confirm))) return
    running.value = true
    try {
      if (await execute(plan, revision)) await options.reload()
    } finally {
      running.value = false
    }
  }

  function dismiss(): void {
    outcome.value = null
  }

  return { outcome, running, canAct, perform, dismiss }
}
