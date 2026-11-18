import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import { SPARK_SAMPLES, sparkShape } from './shape'
import Sparkline from './Sparkline.vue'

describe('Sparkline', () => {
  it('draws a path with a text summary for assistive technology', () => {
    const wrapper = mount(Sparkline, { props: { series: [1, 4, undefined, 2], label: 'Requests per second' } })
    const svg = wrapper.get('svg')
    expect(svg.attributes('role')).toBe('img')
    expect(svg.attributes('aria-label')).toBe('Requests per second: now 2, peak 4, last 3 samples')
    expect(svg.get('path').attributes('d')).toMatch(/^M/)
  })

  it('draws nothing before two samples are known, and says "unknown" for an unknown latest sample', () => {
    expect(mount(Sparkline, { props: { series: [undefined, 3], label: 'x' } }).find('svg').exists()).toBe(false)
    const wrapper = mount(Sparkline, { props: { series: [1, 2, undefined], label: 'x' } })
    expect(wrapper.get('svg').attributes('aria-label')).toContain('now unknown')
  })

  it('formats the summary with the caller`s formatter', () => {
    const wrapper = mount(Sparkline, { props: { series: [1, 2], label: 'Rate', format: (value: number) => `${value}/s` } })
    expect(wrapper.get('svg').attributes('aria-label')).toBe('Rate: now 2/s, peak 2/s, last 2 samples')
  })
})

describe('sparkShape', () => {
  it('has no shape below two known samples', () => {
    expect(sparkShape([])).toBeUndefined()
    expect(sparkShape([undefined, 4, undefined])).toBeUndefined()
  })

  it('draws known samples on a zero baseline, right-aligned, with gaps for unknown ones', () => {
    const shape = sparkShape([0, 10, undefined, 5])!
    expect(shape.known).toEqual([0, 10, 5])
    expect(shape.max).toBe(10)
    expect(shape.last).toBe(5)
    expect(shape.d.match(/M/g)).toHaveLength(2)
    const ys = [...shape.d.matchAll(/[ML][\d.]+ ([\d.]+)/g)].map((match) => Number(match[1]))
    expect(ys[0]).toBeGreaterThan(ys[1]!)
    expect(shape.d.startsWith('M')).toBe(true)
  })

  it('draws a flat series on the baseline and keeps only the last samples', () => {
    expect(sparkShape([0, 0, 0])!.d).toMatch(/^M[\d.]+ 14\.5L/)
    const long = Array.from({ length: SPARK_SAMPLES + 40 }, (_, index) => index)
    expect(sparkShape(long)!.known).toHaveLength(SPARK_SAMPLES)
  })
})
