import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Badge from './Badge.vue'
import StatusDot from './StatusDot.vue'
import { ICON_PATHS, TONES } from './tone'

describe('Badge', () => {
  it('shows the word and a shape, and the colour only as a class', () => {
    const wrapper = mount(Badge, { props: { tone: 'bad', label: 'DOWN' } })
    expect(wrapper.text()).toBe('DOWN')
    expect(wrapper.get('svg').attributes('aria-hidden')).toBe('true')
    expect(wrapper.get('svg').attributes('focusable')).toBe('false')
    expect(wrapper.get('svg path').attributes('d')).toBe(ICON_PATHS.bad)
    expect(wrapper.classes()).toContain('ox-tone-bad')
  })

  it('draws a different shape for each tone, so two tones never differ by colour alone', () => {
    expect(new Set(Object.values(ICON_PATHS)).size).toBe(TONES.length)
    for (const tone of TONES) expect(mount(Badge, { props: { tone, label: tone } }).get('path').attributes('d')).toBe(ICON_PATHS[tone])
  })

  it('renders the label as text, never as markup', () => {
    const wrapper = mount(Badge, { props: { tone: 'warn', label: '<b>EXPIRES SOON</b>' } })
    expect(wrapper.text()).toBe('<b>EXPIRES SOON</b>')
    expect(wrapper.find('b').exists()).toBe(false)
  })
})

describe('StatusDot', () => {
  it('is a decorative mark: the status stays in the text next to it', () => {
    const wrapper = mount(StatusDot, { props: { tone: 'ok' } })
    expect(wrapper.attributes('aria-hidden')).toBe('true')
    expect(wrapper.classes()).toEqual(['ox-dot', 'ox-tone-ok'])
    expect(wrapper.text()).toBe('')
  })
})
