import { describe, expect, it } from 'vitest'
import { mount } from '../test/mount'
import Facts from './Facts.vue'
import { plural } from './plural'
import ScrollRegion from './ScrollRegion.vue'
import WorkspaceSection from './WorkspaceSection.vue'

describe('plural', () => {
  it('counts one item in the singular and groups thousands', () => {
    expect(plural(0, 'item')).toBe('0 items')
    expect(plural(1, 'item')).toBe('1 item')
    expect(plural(1200, 'item')).toBe('1,200 items')
    expect(plural(2, 'entry', 'entries')).toBe('2 entries')
  })
})

describe('WorkspaceSection', () => {
  it('labels the section with its heading and renders description, actions and content', () => {
    const wrapper = mount(WorkspaceSection, {
      props: { title: 'Events', description: 'Recent events.', busy: true },
      slots: { actions: '<b>go</b>', default: 'body text' },
    })

    const heading = wrapper.get('h2')
    expect(wrapper.attributes('aria-labelledby')).toBe(heading.attributes('id'))
    expect(wrapper.attributes('aria-busy')).toBe('true')
    expect(wrapper.text()).toContain('Recent events.')
    expect(wrapper.text()).toContain('body text')
  })

  it('is not busy by default', () => {
    const wrapper = mount(WorkspaceSection, { props: { title: 'Audit', description: 'Records.' } })
    expect(wrapper.attributes('aria-busy')).not.toBe('true')
  })
})

describe('ScrollRegion', () => {
  it('is a labelled, focusable region so a wide table scrolls from the keyboard', () => {
    const wrapper = mount(ScrollRegion, { props: { label: 'Audit records' }, slots: { default: 'rows' } })
    expect(wrapper.attributes()).toMatchObject({ role: 'region', tabindex: '0', 'aria-label': 'Audit records' })
    expect(wrapper.text()).toBe('rows')
  })
})

describe('Facts', () => {
  it('lists each label with its value and marks code values', () => {
    const wrapper = mount(Facts, { props: { items: [{ label: 'Source', value: 'api' }, { label: 'Revision', value: 'abc', code: true }] } })
    expect(wrapper.findAll('dt').map((term) => term.text())).toEqual(['Source', 'Revision'])
    expect(wrapper.findAll('dd').map((value) => value.classes().includes('ox-mono'))).toEqual([false, true])
  })
})
