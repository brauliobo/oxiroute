import { afterEach, describe, expect, it, vi } from 'vitest'
import { clearToasts, toasts } from '#design/toast/state'
import { ApiError } from '../api'
import { pendingConfirm, settleConfirm } from '../shell/confirm'
import { mount } from '../test/mount'
import ActionFeedback from './ActionFeedback.vue'
import { useActionRunner } from './actions'
import type { ActionOutcome, ActionPlan } from './actions'

afterEach(() => {
  clearToasts()
  settleConfirm(false)
})

describe('ActionFeedback', () => {
  const outcome = (over: Partial<ActionOutcome>): ActionOutcome => ({
    scope: 'servers', tone: 'ok', summary: 'Done it.', details: ['A detail.'], ...over,
  })

  it('announces a success in the status region with its details', () => {
    const wrapper = mount(ActionFeedback, { props: { outcome: outcome({}), scope: 'servers' } })
    expect(wrapper.get('[role="status"]').text()).toContain('Done it.')
    expect(wrapper.get('[role="status"]').text()).toContain('A detail.')
    expect(wrapper.get('[role="alert"]').text()).toBe('')
  })

  it('announces a failure in the alert region', () => {
    const wrapper = mount(ActionFeedback, { props: { outcome: outcome({ tone: 'bad', summary: 'Could not x.' }), scope: 'servers' } })
    expect(wrapper.get('[role="alert"]').text()).toContain('Could not x.')
    expect(wrapper.get('[role="status"]').text()).toBe('')
  })

  it('shows nothing for another scope', () => {
    const wrapper = mount(ActionFeedback, { props: { outcome: outcome({}), scope: 'generation' } })
    expect(wrapper.text()).toBe('')
  })

  it('emits dismiss', async () => {
    const wrapper = mount(ActionFeedback, { props: { outcome: outcome({}), scope: 'servers' } })
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('dismiss')).toHaveLength(1)
  })
})

describe('useActionRunner', () => {
  const plan = (over: Partial<ActionPlan<'cams', string, string>> = {}): ActionPlan<'cams', string, string> => ({
    scope: 'cams',
    failure: 'stop the recorder',
    run: async (context) => `ran with ${context}`,
    succeeded: (result) => ({ tone: 'ok', summary: `Recorder stopped (${result}).` }),
    ...over,
  })

  const runner = (over: { context?: () => string | null; reload?: () => Promise<void>; onUnauthorized?: () => void } = {}) =>
    useActionRunner<'cams', string>({
      token: () => 'token',
      context: () => 'ctx',
      conflictHint: 'State changed; reloaded.',
      reload: async () => undefined,
      onUnauthorized: () => undefined,
      ...over,
    })

  it('keeps the success until it is dismissed, announces it as a toast and reloads', async () => {
    const reload = vi.fn(async () => undefined)
    const { outcome, perform, dismiss } = runner({ reload })
    await perform(plan())
    expect(outcome.value).toEqual({ scope: 'cams', tone: 'ok', summary: 'Recorder stopped (ran with ctx).', details: [] })
    expect(toasts.value.map(({ text }) => text)).toEqual(['Recorder stopped (ran with ctx).'])
    expect(reload).toHaveBeenCalledTimes(1)
    dismiss()
    expect(outcome.value).toBeNull()
  })

  it('names the failure, keeps it and does not reload', async () => {
    const reload = vi.fn(async () => undefined)
    const { outcome, perform } = runner({ reload })
    await perform(plan({ run: async () => { throw new ApiError(500, 'Disk is full.', {}) } }))
    expect(outcome.value?.summary).toBe('Could not stop the recorder: Disk is full.')
    expect(outcome.value?.tone).toBe('bad')
    expect(reload).not.toHaveBeenCalled()
  })

  it('reloads after a conflict and adds the hint', async () => {
    const reload = vi.fn(async () => undefined)
    const { outcome, perform } = runner({ reload })
    await perform(plan({ run: async () => { throw new ApiError(409, 'Recording is active.', {}) } }))
    expect(outcome.value?.details).toEqual(['State changed; reloaded.'])
    expect(reload).toHaveBeenCalledTimes(1)
  })

  it('signs out on 401', async () => {
    const onUnauthorized = vi.fn()
    const { perform } = runner({ onUnauthorized })
    await perform(plan({ run: async () => { throw new ApiError(401, 'No.', {}) } }))
    expect(onUnauthorized).toHaveBeenCalledTimes(1)
  })

  it('asks first and sends nothing when the confirmation is declined', async () => {
    const run = vi.fn(async () => 'x')
    const { perform } = runner()
    const pending = perform(plan({ run, confirm: { title: 'Stop it', message: 'Really?', confirmLabel: 'Stop' } }))
    expect(pendingConfirm.value?.title).toBe('Stop it')
    settleConfirm(false)
    await pending
    expect(run).not.toHaveBeenCalled()
  })

  it('does nothing without a token context', async () => {
    const run = vi.fn(async () => 'x')
    const { perform, canAct } = runner({ context: () => null })
    await perform(plan({ run }))
    expect(canAct.value).toBe(false)
    expect(run).not.toHaveBeenCalled()
  })
})
