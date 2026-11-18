import { describe, expect, it } from 'vitest'
import '../../test/dialog'
import { mount } from '../../test/mount'
import Actions from './Actions.vue'
import Dialog from './Dialog.vue'

const content = () => {
  const button = document.createElement('button')
  button.dataset.close = ''
  button.textContent = 'Close'
  return button
}

describe('Dialog', () => {
  it('is a closed native dialog named by its own heading', () => {
    const wrapper = mount(Dialog, { props: { open: false, title: 'Confirm' }, slots: { default: content } })
    const heading = wrapper.get('h2')
    expect(wrapper.element.tagName).toBe('DIALOG')
    expect(wrapper.attributes('open')).toBeUndefined()
    expect(wrapper.attributes('aria-labelledby')).toBe(heading.attributes('id'))
    expect(heading.text()).toBe('Confirm')
  })

  it('opens modally with the open prop and closes through data-close, telling the page', async () => {
    const wrapper = mount(Dialog, { props: { open: false, title: 'Confirm' }, slots: { default: content } })
    await wrapper.setProps({ open: true })
    expect(wrapper.attributes('open')).toBeDefined()
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('close')).toHaveLength(1)
    expect(wrapper.attributes('open')).toBeUndefined()
  })

  it('closes on a click on the backdrop but not on a click in its content', async () => {
    const wrapper = mount(Dialog, { props: { open: true, title: 'Confirm' }, slots: { default: 'text' } })
    await wrapper.get('h2').trigger('click')
    expect(wrapper.attributes('open')).toBeDefined()
    await wrapper.trigger('click')
    expect(wrapper.attributes('open')).toBeUndefined()
  })

  it('tells the page when the browser closes it (Escape)', () => {
    const wrapper = mount(Dialog, { props: { open: true, title: 'Confirm' } })
    ;(wrapper.element as HTMLDialogElement).close()
    expect(wrapper.emitted('close')).toHaveLength(1)
  })
})

describe('Actions', () => {
  it('holds the buttons of a dialog', () => {
    const wrapper = mount(Actions, { slots: { default: content } })
    expect(wrapper.classes()).toContain('ox-dialog-actions')
    expect(wrapper.find('button').exists()).toBe(true)
  })
})
