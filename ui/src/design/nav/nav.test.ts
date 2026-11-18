import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Nav from './Nav.vue'

const ITEMS = [
  { id: 'overview', label: 'Overview', href: '#/overview' },
  { id: 'audit', label: 'Audit', href: '#/audit' },
]

describe('Nav', () => {
  it('is a labelled navigation landmark with one link per item', () => {
    const wrapper = mount(Nav, { props: { items: ITEMS, current: 'audit', label: 'Console sections' } })
    expect(wrapper.element.tagName).toBe('NAV')
    expect(wrapper.attributes('aria-label')).toBe('Console sections')
    expect(wrapper.findAll('a').map((link) => [link.text(), link.attributes('href')])).toEqual([
      ['Overview', '#/overview'], ['Audit', '#/audit'],
    ])
  })

  it('marks only the current page, for assistive technology and not by colour alone', async () => {
    const wrapper = mount(Nav, { props: { items: ITEMS, current: 'audit', label: 'Console sections' } })
    expect(wrapper.findAll('a').map((link) => link.attributes('aria-current'))).toEqual([undefined, 'page'])
    await wrapper.setProps({ current: 'overview' })
    expect(wrapper.findAll('a').map((link) => link.attributes('aria-current'))).toEqual(['page', undefined])
  })
})
