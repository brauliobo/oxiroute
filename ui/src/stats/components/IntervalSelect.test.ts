import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import IntervalSelect from './IntervalSelect.vue'

const options = (wrapper: ReturnType<typeof mount>) => wrapper.findAll('option').map((option) => option.text())

describe('IntervalSelect', () => {
  it('follows the server hint by default', () => {
    const wrapper = mount(IntervalSelect, { props: { modelValue: undefined, hintMs: 2500 } })
    expect(wrapper.get('label').text()).toBe('Refresh every')
    expect(options(wrapper)).toEqual(['Server hint (2.5 s)', '1 s', '2 s', '5 s', '10 s', '30 s', '60 s'])
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('')
  })

  it('emits the chosen milliseconds, or undefined to follow the server again', async () => {
    const wrapper = mount(IntervalSelect, { props: { modelValue: 5000, hintMs: 2000 } })
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('5000')
    await wrapper.get('select').setValue('10000')
    await wrapper.get('select').setValue('')
    expect(wrapper.emitted('update:modelValue')).toEqual([[10_000], [undefined]])
  })

  it('keeps an interval from a shared link that is not a preset', () => {
    const wrapper = mount(IntervalSelect, { props: { modelValue: 3000, hintMs: 2000 } })
    expect(options(wrapper)).toContain('3 s')
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('3000')
  })
})
