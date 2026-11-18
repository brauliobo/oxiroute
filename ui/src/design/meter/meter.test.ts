import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Meter from './Meter.vue'

const fill = (wrapper: ReturnType<typeof mount>) => (wrapper.get('.ox-meter-fill').element as HTMLElement).style.getPropertyValue('--ox-w')

describe('Meter', () => {
  it('is a labelled meter whose fill is a share of its maximum, set through the CSSOM', () => {
    const wrapper = mount(Meter, { props: { value: 42, max: 168, label: 'Connections', valueText: '42 of 168' } })
    expect(wrapper.attributes('role')).toBe('meter')
    expect(wrapper.attributes('aria-label')).toBe('Connections')
    expect(wrapper.attributes('aria-valuemin')).toBe('0')
    expect(wrapper.attributes('aria-valuemax')).toBe('168')
    expect(wrapper.attributes('aria-valuenow')).toBe('42')
    expect(wrapper.attributes('aria-valuetext')).toBe('42 of 168')
    expect(fill(wrapper)).toBe('25')
    expect(wrapper.get('.ox-meter-fill').attributes('style') ?? '').not.toContain('width')
  })

  it('is a percentage by default and never draws outside its track', async () => {
    const wrapper = mount(Meter, { props: { value: 130, label: 'Load' } })
    expect(wrapper.attributes('aria-valuemax')).toBe('100')
    expect(fill(wrapper)).toBe('100')
    await wrapper.setProps({ value: -5 })
    expect(fill(wrapper)).toBe('0')
  })

  it('takes its colour from the tone and an empty range stays empty', () => {
    const wrapper = mount(Meter, { props: { value: 3, max: 0, label: 'Empty', tone: 'bad' } })
    expect(wrapper.classes()).toContain('ox-tone-bad')
    expect(fill(wrapper)).toBe('0')
  })
})
