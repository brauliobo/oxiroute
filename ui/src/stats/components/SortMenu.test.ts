import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import SortMenu from './SortMenu.vue'

describe('SortMenu', () => {
  it('offers the default order and every sortable column', () => {
    const wrapper = mount(SortMenu, { props: { sort: undefined } })
    expect(wrapper.get('label').text()).toBe('Sort by')
    expect(wrapper.findAll('option').map((option) => option.text()))
      .toEqual(['Default order', 'Name', 'Status', 'Active', 'Rate', 'Errors', 'p99', 'Received', 'Sent'])
    expect(wrapper.get('button').attributes('disabled')).toBeDefined()
  })

  it('emits the column keeping the direction, and no sort for the default order', async () => {
    const wrapper = mount(SortMenu, { props: { sort: { column: 'name', direction: 'desc' } } })
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('name')
    await wrapper.get('select').setValue('rate')
    await wrapper.get('select').setValue('')
    expect(wrapper.emitted('update:sort')).toEqual([[{ column: 'rate', direction: 'desc' }], [undefined]])
  })

  it('starts a chosen column ascending and flips the direction with a pressed button', async () => {
    const wrapper = mount(SortMenu, { props: { sort: undefined } })
    await wrapper.get('select').setValue('errors')
    expect(wrapper.emitted('update:sort')![0]).toEqual([{ column: 'errors', direction: 'asc' }])
    await wrapper.setProps({ sort: { column: 'errors', direction: 'asc' } })
    const button = wrapper.get('button')
    expect(button.text()).toBe('Descending')
    expect(button.attributes('aria-pressed')).toBe('false')
    await button.trigger('click')
    expect(wrapper.emitted('update:sort')![1]).toEqual([{ column: 'errors', direction: 'desc' }])
  })
})
