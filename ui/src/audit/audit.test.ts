import { describe, expect, it } from 'vitest'
import { mount } from '../test/mount'
import AuditFilters from './AuditFilters.vue'
import { CATEGORY_OPTIONS, RESULT_OPTIONS, connectionBadge, degradedCheck, resultTone, storeBadge } from './model'

describe('audit model', () => {
  it('words options in sentence case with an all-entry first', () => {
    expect(CATEGORY_OPTIONS.map(({ label }) => label)).toEqual(['All categories', 'Reload', 'Import', 'Certificate', 'Control'])
    expect(RESULT_OPTIONS[0]).toEqual({ value: '', label: 'All results' })
    expect(RESULT_OPTIONS.map(({ value }) => value)).toContain('conflict')
  })

  it('gives each result and state a tone', () => {
    expect(resultTone('succeeded')).toBe('ok')
    expect(resultTone('requested')).toBe('info')
    expect(resultTone('partial')).toBe('warn')
    expect(resultTone('rejected')).toBe('bad')
    expect(storeBadge('memory')).toEqual({ tone: 'warn', label: 'Memory only' })
    expect(connectionBadge('unavailable').tone).toBe('bad')
  })

  it('maps the store error codes to what to check', () => {
    expect(degradedCheck('root_mode_unsafe')).toContain('mode 0700')
    expect(degradedCheck('unsafe_entry')).toContain('mode 0600')
    expect(degradedCheck('io_failed')).toContain('free space')
    expect(degradedCheck('anything_else')).toContain('daemon log')
    expect(degradedCheck(undefined)).toContain('daemon log')
  })
})

describe('AuditFilters', () => {
  const none = { category: '', result: '' }

  it('emits the drafted selection on submit and the clear request', async () => {
    const wrapper = mount(AuditFilters, { props: { applied: none, disabled: false } })

    await wrapper.get('#audit-category').setValue('import')
    await wrapper.get('#audit-result').setValue('failed')
    await wrapper.get('form').trigger('submit')
    expect(wrapper.emitted('apply')).toEqual([[{ category: 'import', result: 'failed' }]])

    await wrapper.findAll('button').find((button) => button.text() === 'Clear filters')!.trigger('click')
    expect(wrapper.emitted('clear')).toHaveLength(1)
  })

  it('follows the applied selection and disables everything while loading', async () => {
    const wrapper = mount(AuditFilters, { props: { applied: none, disabled: true } })
    expect(wrapper.findAll('button').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
    expect(wrapper.get('#audit-category').attributes('disabled')).toBeDefined()

    await wrapper.setProps({ applied: { category: 'reload', result: '' }, disabled: false })
    expect((wrapper.get('#audit-category').element as HTMLSelectElement).value).toBe('reload')
  })
})
