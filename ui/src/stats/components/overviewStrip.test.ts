import { describe, expect, it } from 'vitest'
import { counter } from '../fixtures'
import OverviewStrip from './OverviewStrip.vue'
import { documentAt, loadedStore, mountView } from './testing'

describe('OverviewStrip', () => {
  it('shows version, uptime, generation, totals, pool health, the soonest certificate and the warnings', async () => {
    const wrapper = mountView(OverviewStrip, await loadedStore([documentAt(0)]))
    const tile = (name: string) => wrapper.findAll('.ox-tile').find((item) => item.get('dt').text() === name)!.text()
    expect(tile('Version')).toContain('6.3.0')
    expect(tile('Version')).toContain('up 1h 0m')
    expect(tile('Generation')).toContain('accepting')
    expect(tile('Generation')).toContain('revision rev-4')
    expect(tile('Connections')).toContain('7 / 4.1k')
    expect(tile('Pools')).toContain('1 of 2 up')
    expect(tile('Pools')).toContain('1 degraded, 0 down')
    expect(tile('Servers')).toContain('2 of 3 up')
    expect(tile('Certificates')).toContain('VALID')
    expect(tile('Certificates')).toContain('edge in 90d 0h')
    expect(wrapper.get('a[href="#warnings"]').text()).toBe('1')
  })

  it('has no rate before two samples, then request, byte and error rates', async () => {
    const harness = await loadedStore([documentAt(0), documentAt(2, (wire) => {
      wire.listeners[0]!.http!.total = counter(5100)
      wire.listeners[0]!.http!.outcomes[3]!.count = counter(30)
      wire.process.bytesSent = counter(64_002_000)
    })])
    const wrapper = mountView(OverviewStrip, harness)
    const tile = (name: string) => wrapper.findAll('.ox-tile').find((item) => item.get('dt').text() === name)!.text()
    expect(tile('Requests')).toContain('—')
    await harness.next()
    expect(tile('Requests')).toContain('100/s')
    expect(tile('Requests')).toContain('errors 10%')
    expect(tile('Traffic')).toContain('out 1000 B/s')
  })

  it('names an expired certificate in words', async () => {
    const wrapper = mountView(OverviewStrip, await loadedStore([documentAt(0, (wire) => {
      wire.certificates[0]!.notAfterUnixSeconds = wire.sampledAtUnixMs / 1000 - 86_400
    })]))
    const text = wrapper.findAll('.ox-tile').find((item) => item.get('dt').text() === 'Certificates')!.text()
    expect(text).toContain('EXPIRED')
    expect(text).toContain('expired 1d 0h ago')
  })

  it('shows nothing about certificates when none is loaded, and the em dash', async () => {
    const wrapper = mountView(OverviewStrip, await loadedStore([documentAt(0, (wire) => { wire.certificates = [] })]))
    expect(wrapper.findAll('.ox-tile').find((item) => item.get('dt').text() === 'Certificates')!.text()).toBe('Certificates—')
  })
})

