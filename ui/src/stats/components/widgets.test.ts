import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import DetailList from './DetailList.vue'
import Histogram from './Histogram.vue'
import Latency from './Latency.vue'
import ResponseBar from './ResponseBar.vue'
import Sparkline from './Sparkline.vue'
import StatusBadge from './StatusBadge.vue'

describe('StatusBadge', () => {
  it('shows the word and a shape, and the colour only as a class', () => {
    const wrapper = mount(StatusBadge, { props: { status: 'down' } })
    expect(wrapper.text()).toBe('DOWN')
    expect(wrapper.get('svg').attributes('aria-hidden')).toBe('true')
    expect(wrapper.get('svg path').attributes('d')).toBeTruthy()
    expect(wrapper.classes()).toContain('ox-tone-bad')
  })

  it('draws a different shape for each tone, so two statuses never differ by colour alone', () => {
    const shape = (status: string) => mount(StatusBadge, { props: { status } }).get('path').attributes('d')
    const shapes = ['up', 'drain', 'down', 'maint', 'unchecked'].map(shape)
    expect(new Set(shapes).size).toBe(5)
  })

  it('takes a label, and renders names as text', () => {
    const wrapper = mount(StatusBadge, { props: { status: 'soon', label: '<b>EXPIRES SOON</b>' } })
    expect(wrapper.text()).toBe('<b>EXPIRES SOON</b>')
    expect(wrapper.find('b').exists()).toBe(false)
  })
})

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
})

describe('ResponseBar', () => {
  const responses = { '1xx': 0n, '2xx': 900n, '3xx': 0n, '4xx': 75n, '5xx': 25n }

  it('shows classes as proportional segments and as numbers', () => {
    const wrapper = mount(ResponseBar, { props: { responses } })
    const segments = wrapper.findAll('.ox-resp-seg')
    expect(segments.map((segment) => (segment.element as HTMLElement).style.getPropertyValue('--ox-w'))).toEqual(['90', '7.5', '2.5'])
    expect(wrapper.findAll('.ox-resp-nums li').map((item) => item.text())).toEqual(['2xx 900', '4xx 75', '5xx 25'])
    expect(wrapper.get('.ox-resp').attributes('title')).toBe('2xx: 900, 4xx: 75, 5xx: 25')
  })

  it('says so instead of drawing an empty bar when nothing was answered yet', () => {
    const wrapper = mount(ResponseBar, { props: { responses: { ...responses, '2xx': 0n, '4xx': 0n, '5xx': 0n } } })
    expect(wrapper.find('.ox-resp-bar').exists()).toBe(false)
    expect(wrapper.text()).toBe('none yet')
  })
})

describe('Histogram', () => {
  it('shows one bar and number per bucket, overflow included', () => {
    const wrapper = mount(Histogram, {
      props: { histogram: { count: 10n, sumMs: 40n, buckets: [1n, 4n, 8n] }, bounds: [1, 5, 10], label: 'Request duration' },
    })
    expect(wrapper.get('figcaption').text()).toBe('Request duration: 10 observations')
    expect(wrapper.findAll('li').map((item) => item.text())).toEqual(['≤ 1 ms1', '≤ 5 ms3', '≤ 10 ms4', '> 10 ms2'])
    expect((wrapper.findAll('.ox-hist-bar')[2]!.element as HTMLElement).style.getPropertyValue('--ox-w')).toBe('100')
  })

  it('shows no bars without observations', () => {
    const wrapper = mount(Histogram, { props: { histogram: { count: 0n, sumMs: 0n, buckets: [0n] }, bounds: [1], label: 'Wait' } })
    expect(wrapper.find('ol').exists()).toBe(false)
    expect(wrapper.text()).toContain('No observations yet.')
  })
})

describe('Latency', () => {
  it('reads bucket bounds, flags the overflow and shows the em dash without observations', () => {
    const summary = { count: 3n, meanMs: 5, p50: { ms: 5, overflow: false }, p90: { ms: 50, overflow: false }, p99: { ms: 5000, overflow: true } }
    const wrapper = mount(Latency, { props: { summary } })
    expect(wrapper.findAll('li').map((item) => item.text())).toEqual(['p50 5 ms', 'p90 50 ms', 'p99 > 5 s'])
    expect(wrapper.findAll('li')[0]!.attributes('title')).toContain('at most')
    expect(mount(Latency).findAll('li').map((item) => item.text())).toEqual(['p50 —', 'p90 —', 'p99 —'])
  })
})

describe('DetailList', () => {
  it('lists labelled values with the em dash for absent ones', () => {
    const wrapper = mount(DetailList, { props: { items: [{ label: 'Address', value: '198.51.100.1:80' }, { label: 'Reason' }] } })
    expect(wrapper.findAll('dt').map((item) => item.text())).toEqual(['Address', 'Reason'])
    expect(wrapper.findAll('dd').map((item) => item.text())).toEqual(['198.51.100.1:80', '—'])
  })
})
