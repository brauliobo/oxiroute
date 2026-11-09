import { nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'

import { vCssVars } from './cssVars'

describe('vCssVars', () => {
  it('writes custom properties through the CSSOM so a strict style-src accepts them', async () => {
    const element = document.createElement('div')

    vCssVars(element, () => ({ '--fill': 42.5, '--label': 'wide' }))
    await nextTick()

    expect(element.style.getPropertyValue('--fill')).toBe('42.5')
    expect(element.style.getPropertyValue('--label')).toBe('wide')
  })

  it('follows reactive values and removes properties that disappear', async () => {
    const element = document.createElement('div')
    const vars = ref<Record<string, number>>({ '--x': 1, '--y': 2 })

    vCssVars(element, () => vars.value)
    await nextTick()
    vars.value = { '--x': 5 }
    await nextTick()

    expect(element.style.getPropertyValue('--x')).toBe('5')
    expect(element.style.getPropertyValue('--y')).toBe('')
  })
})
