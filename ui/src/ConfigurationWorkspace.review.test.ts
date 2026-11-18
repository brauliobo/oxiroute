import { flushPromises } from './test/mount'
import { describe, expect, it } from 'vitest'

import { findButton, installConfigFetch, mountUnlocked } from './test/configurationWorkspace'

async function openReview(version: string | null) {
  installConfigFetch()
  const wrapper = await mountUnlocked(true)
  if (version) await wrapper.get('[data-field="version"] input').setValue(version)
  await findButton(wrapper, 'Validate candidate').trigger('click')
  await flushPromises()
  await findButton(wrapper, 'Review save').trigger('click')
  await flushPromises()
  return wrapper
}

describe('save review diff', () => {
  it('lists removed and added rendered lines with words and symbols, not colour alone', async () => {
    const wrapper = await openReview('2')

    const lines = wrapper.findAll('.ox-diff-line')
    expect(lines.map((line) => line.attributes('data-kind'))).toEqual(['removed', 'added'])
    expect(lines[0]!.text()).toContain('Removed:')
    expect(lines[0]!.text()).toContain('version 1')
    expect(lines[1]!.text()).toContain('Added:')
    expect(lines[1]!.text()).toContain('version 2')
    expect(lines[0]!.get('.ox-diff-mark').text()).toBe('-')
    expect(lines[1]!.get('.ox-diff-mark').text()).toBe('+')
    expect(wrapper.get('.ox-diff-summary').text()).toBe('1 line added (+), 1 line removed (-)')
  })

  it('says so when the candidate renders exactly like the disk file', async () => {
    const wrapper = await openReview(null)

    expect(wrapper.find('.ox-diff-line').exists()).toBe(false)
    expect(wrapper.get('.ox-diff-summary').text()).toContain('No line changes')
  })

  it('names the scrollable diff region so it is reachable by keyboard', async () => {
    const wrapper = await openReview('3')

    const region = wrapper.get('.ox-diff-scroll')
    expect(region.attributes('role')).toBe('region')
    expect(region.attributes('aria-label')).toBe('Configuration diff')
    expect(region.attributes('tabindex')).toBe('0')
  })
})
