import { describe, expect, it } from 'vitest'
import '../../test/dialog'
import { mount } from '../../test/mount'
import { SHORTCUTS } from '../shortcuts'
import ShortcutsDialog from './ShortcutsDialog.vue'

describe('ShortcutsDialog', () => {
  it('is a closed, labelled modal dialog that lists every shortcut', () => {
    const wrapper = mount(ShortcutsDialog, { props: { open: false } })
    const dialog = wrapper.get('dialog')
    expect(dialog.attributes('open')).toBeUndefined()
    const heading = wrapper.get('h2')
    expect(dialog.attributes('aria-labelledby')).toBe(heading.attributes('id'))
    expect(heading.text()).toBe('Keyboard shortcuts')
    expect(wrapper.findAll('kbd').map((key) => key.text())).toEqual(SHORTCUTS.map((shortcut) => shortcut.keys))
  })

  it('opens and closes with the open prop and tells when the viewer closes it', async () => {
    const wrapper = mount(ShortcutsDialog, { props: { open: false } })
    await wrapper.setProps({ open: true })
    expect(wrapper.get('dialog').attributes('open')).toBeDefined()
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('close')).toHaveLength(1)
    expect(wrapper.get('dialog').attributes('open')).toBeUndefined()
  })

  it('closes when the viewer presses Escape in the dialog (the browser closes it)', async () => {
    const wrapper = mount(ShortcutsDialog, { props: { open: true } })
    ;(wrapper.get('dialog').element as HTMLDialogElement).close()
    expect(wrapper.emitted('close')).toHaveLength(1)
  })
})
