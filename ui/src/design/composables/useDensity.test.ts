import { nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'
import { DENSITIES, applyDensity, useDensity } from './useDensity'
import type { Density } from './useDensity'

describe('density', () => {
  it('sets data-density on the root element', () => {
    const root = document.createElement('html')
    applyDensity('comfortable', root)
    expect(root.dataset.density).toBe('comfortable')
  })

  it('offers compact for dense data pages and comfortable for the console', () => {
    expect(DENSITIES).toEqual(['compact', 'comfortable'])
  })

  it('keeps <html data-density> in step with a reactive density', async () => {
    const density = ref<Density>('compact')
    useDensity(density)
    expect(document.documentElement.dataset.density).toBe('compact')
    density.value = 'comfortable'
    await nextTick()
    expect(document.documentElement.dataset.density).toBe('comfortable')
  })
})
