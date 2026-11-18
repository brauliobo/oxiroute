import { afterEach, describe, expect, it, vi } from 'vitest'
import { nextTick } from 'vue'
import { mount } from '../../test/mount'
import Region from './Region.vue'
import Toast from './Toast.vue'
import { clearToasts, dismissToast, pushToast, toasts } from './state'

afterEach(() => {
  clearToasts()
  vi.useRealTimers()
})

describe('Toast', () => {
  it('announces politely, but an error as an alert, and can be dismissed', async () => {
    const info = mount(Toast, { props: { tone: 'info', text: 'Saved' } })
    expect(info.attributes('role')).toBe('status')
    expect(info.get('span').text()).toBe('Saved')
    await info.get('button').trigger('click')
    expect(info.emitted('dismiss')).toHaveLength(1)
    const bad = mount(Toast, { props: { tone: 'bad', text: 'Failed' } })
    expect(bad.attributes('role')).toBe('alert')
    expect(bad.attributes('data-tone')).toBe('bad')
  })
})

describe('toast state', () => {
  it('keeps toasts in order, dismisses one by id and clears all', () => {
    const first = pushToast('one')
    pushToast('two', 'warn')
    expect(toasts.value.map((toast) => [toast.text, toast.tone])).toEqual([['one', 'info'], ['two', 'warn']])
    dismissToast(first)
    expect(toasts.value.map((toast) => toast.text)).toEqual(['two'])
    clearToasts()
    expect(toasts.value).toEqual([])
  })

  it('removes a toast after its time to live', () => {
    vi.useFakeTimers()
    pushToast('brief', 'ok', 3000)
    pushToast('sticky')
    vi.advanceTimersByTime(3000)
    expect(toasts.value.map((toast) => toast.text)).toEqual(['sticky'])
  })
})

describe('Region', () => {
  it('lists the pending toasts in a labelled region and drops a dismissed one', async () => {
    const wrapper = mount(Region)
    expect(wrapper.attributes('aria-label')).toBe('Notifications')
    expect(wrapper.findAll('.ox-toast')).toHaveLength(0)
    pushToast('Saved', 'ok')
    pushToast('Failed', 'bad')
    await nextTick()
    expect(wrapper.findAll('.ox-toast').map((toast) => toast.get('span').text())).toEqual(['Saved', 'Failed'])
    await wrapper.findAll('.ox-toast')[0]!.get('button').trigger('click')
    expect(wrapper.findAll('.ox-toast').map((toast) => toast.get('span').text())).toEqual(['Failed'])
  })
})
