import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import SortHeader from './SortHeader.vue'
import Table from './Table.vue'

const row = () => {
  const body = document.createElement('tbody')
  body.innerHTML = '<tr><th scope="row">web</th><td>up</td></tr>'
  return body
}

describe('Table', () => {
  it('has a screen-reader caption, scoped column headers and the rows it is given', () => {
    const wrapper = mount(Table, { props: { caption: 'Pools: name and state', columns: ['Pool', 'State'] }, slots: { default: row } })
    expect(wrapper.element.tagName).toBe('TABLE')
    expect(wrapper.get('caption').text()).toBe('Pools: name and state')
    expect(wrapper.get('caption').classes()).toContain('ox-sr-only')
    expect(wrapper.findAll('thead th').map((cell) => [cell.text(), cell.attributes('scope')])).toEqual([['Pool', 'col'], ['State', 'col']])
    expect(wrapper.get('tbody th').attributes('scope')).toBe('row')
  })

  it('takes custom header cells in the head slot', () => {
    const head = () => {
      const tr = document.createElement('tr')
      tr.innerHTML = '<th scope="col">Custom</th>'
      return tr
    }
    const wrapper = mount(Table, { props: { caption: 'x' }, slots: { head } })
    expect(wrapper.findAll('thead th').map((cell) => cell.text())).toEqual(['Custom'])
  })
})

describe('SortHeader', () => {
  const header = (props: Record<string, unknown>) => mount(SortHeader, { props: { label: 'Rate', ...props } })

  it('is a plain column header unless sortable', () => {
    const wrapper = header({})
    expect(wrapper.element.tagName).toBe('TH')
    expect(wrapper.find('button').exists()).toBe(false)
    expect(wrapper.attributes('aria-sort')).toBeUndefined()
    expect(wrapper.text()).toBe('Rate')
  })

  it('announces the sort direction and tells when the viewer asks to sort', async () => {
    const wrapper = header({ sortable: true, direction: 'ascending' })
    expect(wrapper.attributes('aria-sort')).toBe('ascending')
    expect(wrapper.get('.ox-sort-mark').text()).toBe('▲')
    expect(wrapper.get('.ox-sort-mark').attributes('aria-hidden')).toBe('true')
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('sort')).toHaveLength(1)
    await wrapper.setProps({ direction: 'descending' })
    expect(wrapper.attributes('aria-sort')).toBe('descending')
    expect(wrapper.get('.ox-sort-mark').text()).toBe('▼')
  })

  it('is not sorted by default when sortable', () => {
    const wrapper = header({ sortable: true })
    expect(wrapper.attributes('aria-sort')).toBe('none')
    expect(wrapper.get('.ox-sort-mark').text()).toBe('')
  })
})
