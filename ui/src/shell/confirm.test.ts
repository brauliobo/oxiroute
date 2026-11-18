import { afterEach, describe, expect, it } from 'vitest'
import '../test/dialog'
import { flushPromises, mount } from '../test/mount'
import { confirmAction, pendingConfirm, settleConfirm } from './confirm'
import ConfirmHost from './ConfirmHost.vue'

afterEach(() => settleConfirm(false))

const request = {
  title: 'Drain pool web-backends',
  message: 'New connections stop; in-flight ones finish.',
  confirmLabel: 'Drain pool',
  details: ['2 servers', '14 active connections'],
}

describe('ConfirmHost', () => {
  it('stays closed until an action asks, then names the target and its consequences', async () => {
    const wrapper = mount(ConfirmHost)
    expect(wrapper.attributes('open')).toBeUndefined()
    void confirmAction(request)
    await flushPromises()
    expect(wrapper.attributes('open')).toBeDefined()
    expect(wrapper.get('h2').text()).toBe('Drain pool web-backends')
    expect(wrapper.text()).toContain('in-flight ones finish')
    expect(wrapper.findAll('li').map((item) => item.text())).toEqual(['2 servers', '14 active connections'])
  })

  it('opens with the cancel button focused', async () => {
    const wrapper = mount(ConfirmHost, { attachTo: document.body })
    void confirmAction(request)
    await flushPromises()
    expect(document.activeElement).toBe(wrapper.get('[data-close]').element)
  })

  it('resolves true on confirm and closes', async () => {
    const wrapper = mount(ConfirmHost)
    const answer = confirmAction({ ...request, danger: true })
    await flushPromises()
    await wrapper.get('[data-confirm]').trigger('click')
    await flushPromises()
    await expect(answer).resolves.toBe(true)
    expect(wrapper.attributes('open')).toBeUndefined()
    expect(pendingConfirm.value).toBeNull()
  })

  it('resolves false on cancel and on the browser closing the dialog', async () => {
    const wrapper = mount(ConfirmHost)
    const cancelled = confirmAction(request)
    await flushPromises()
    await wrapper.get('[data-close]').trigger('click')
    await expect(cancelled).resolves.toBe(false)

    const escaped = confirmAction(request)
    await flushPromises()
    ;(wrapper.element as HTMLDialogElement).close()
    await expect(escaped).resolves.toBe(false)
  })

  it('asks for the target to be typed before it enables a dangerous confirm', async () => {
    const wrapper = mount(ConfirmHost)
    const answer = confirmAction({ ...request, confirmLabel: 'Shut down', typeToConfirm: 'shutdown' })
    await flushPromises()
    const confirm = wrapper.get('[data-confirm]')
    expect(confirm.attributes('disabled')).toBeDefined()
    await wrapper.get('#ox-confirm-typed').setValue('shutdow')
    expect(confirm.attributes('disabled')).toBeDefined()
    await wrapper.get('#ox-confirm-typed').setValue('shutdown')
    expect(confirm.attributes('disabled')).toBeUndefined()
    await confirm.trigger('click')
    await expect(answer).resolves.toBe(true)
  })

  it('cancels the previous question when a new one is asked', async () => {
    mount(ConfirmHost)
    const first = confirmAction(request)
    void confirmAction({ ...request, title: 'Second' })
    await expect(first).resolves.toBe(false)
  })
})
