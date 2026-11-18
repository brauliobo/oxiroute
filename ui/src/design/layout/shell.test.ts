import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Shell from './Shell.vue'
import ThemeToggle from './ThemeToggle.vue'

const element = (html: string) => () => {
  const template = document.createElement('template')
  template.innerHTML = html
  return template.content.firstElementChild!
}

describe('Shell', () => {
  it('has a skip link to the single main landmark, one h1 and a header', () => {
    const wrapper = mount(Shell, { props: { title: 'Console', skipLabel: 'Skip to content' }, slots: { default: 'content' } })
    expect(wrapper.findAll('h1')).toHaveLength(1)
    expect(wrapper.get('header').text()).toContain('Console')
    expect(wrapper.get('a').attributes('href')).toBe('#ox-main')
    expect(wrapper.get('a').text()).toBe('Skip to content')
    expect(wrapper.findAll('main')).toHaveLength(1)
    expect(wrapper.get('main').attributes('id')).toBe('ox-main')
    expect(wrapper.get('main').attributes('tabindex')).toBe('-1')
    expect(wrapper.get('main').text()).toBe('content')
  })

  it('takes a different main id and puts the slots in reading order', () => {
    const wrapper = mount(Shell, {
      props: { title: 'Stats', skipLabel: 'Skip', mainId: 'stats-main' },
      slots: {
        header: element('<span id="status">Live</span>'),
        above: element('<div id="toolbar"></div>'),
        default: element('<p id="data"></p>'),
        footer: element('<footer id="foot"></footer>'),
      },
    })
    expect(wrapper.get('a').attributes('href')).toBe('#stats-main')
    const order = [...wrapper.element.querySelectorAll('[id]')].map((node) => node.id)
    expect(order).toEqual(['status', 'toolbar', 'stats-main', 'data', 'foot'])
    expect(wrapper.get('header #status').text()).toBe('Live')
  })
})

describe('Shell theme toggle', () => {
  it('offers a theme choice in the header only when given a theme', async () => {
    const without = mount(Shell, { props: { title: 'Console', skipLabel: 'Skip' } })
    expect(without.find('select').exists()).toBe(false)
    const wrapper = mount(Shell, { props: { title: 'Console', skipLabel: 'Skip', theme: 'auto' } })
    await wrapper.get('header select').setValue('dark')
    expect(wrapper.emitted('update:theme')).toEqual([['dark']])
  })
})

describe('ThemeToggle', () => {
  it('chooses between automatic, light and dark', async () => {
    const wrapper = mount(ThemeToggle, { props: { modelValue: 'light' } })
    expect(wrapper.get('label').text()).toBe('Theme')
    expect(wrapper.get('label').attributes('for')).toBe(wrapper.get('select').attributes('id'))
    expect(wrapper.findAll('option').map((option) => option.text())).toEqual(['Automatic', 'Light', 'Dark'])
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('light')
    await wrapper.get('select').setValue('dark')
    expect(wrapper.emitted('update:modelValue')).toEqual([['dark']])
  })
})
