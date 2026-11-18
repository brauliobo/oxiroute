import { describe, expect, it } from 'vitest'
import { useLiveRegion } from './useLiveRegion'

describe('useLiveRegion', () => {
  it('announces politely through one visually hidden status region', () => {
    const { announce } = useLiveRegion()
    announce('Saved')
    announce('Saved again')
    const regions = document.querySelectorAll('[role="status"][aria-live="polite"]')
    expect(regions).toHaveLength(1)
    expect(regions[0]!.textContent).toBe('Saved again')
    expect(regions[0]!.classList.contains('ox-sr-only')).toBe(true)
  })

  it('keeps urgent messages in a separate assertive alert region', () => {
    const { announce } = useLiveRegion()
    announce('Connection lost', 'assertive')
    expect(document.querySelector('[role="alert"][aria-live="assertive"]')!.textContent).toBe('Connection lost')
  })

  it('creates the region again when the page replaced it', () => {
    const { announce } = useLiveRegion()
    announce('one')
    document.body.innerHTML = ''
    announce('two')
    expect(document.querySelector('[role="status"]')!.textContent).toBe('two')
  })
})
