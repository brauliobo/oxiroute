import { describe, expect, it } from 'vitest'
import type { OperationalEvent } from '../api'
import { mount } from '../test/mount'
import { ALL_EVENTS, eventNameOptions, filterEvents, ringRestarted } from './filter'
import { eventNotes, outcomeDetail, outcomeLabel, outcomeTone, sentence } from './format'
import StreamStatus from './StreamStatus.vue'

const base = { cursor: 1, timestampUnixMs: null, revision: null }
const failed = { ...base, event: 'server_update', outcome: 'failed' } as OperationalEvent
const ejected = {
  ...base,
  cursor: 2,
  event: 'upstream_endpoint_ejection',
  outcome: {
    type: 'ejected', pool: 'web', server: 'a', reason: 'connect_failed',
    failureCount: 1, ejectionCount: 1, ejectedAtUnixMs: 1, ejectionUntilUnixMs: 2,
  },
} as OperationalEvent

describe('event presentation', () => {
  it('words names and outcomes in sentence case with a tone each', () => {
    expect(sentence('generation_prepare')).toBe('Generation prepare')
    expect(outcomeLabel('forced_close')).toBe('Forced close')
    expect(outcomeTone('failed')).toBe('bad')
    expect(outcomeTone('applied')).toBe('ok')
    expect(outcomeTone('retiring')).toBe('info')
    expect(outcomeTone(ejected.outcome)).toBe('bad')
  })

  it('describes structured outcomes without object coercion', () => {
    expect(outcomeDetail('applied')).toBeNull()
    expect(outcomeDetail(ejected.outcome)).toBe('web / a / connect failed')
    expect(eventNotes({ ...ejected, revision: 'abc', certificate: 'edge' })).toEqual([
      'Certificate edge', 'web / a / connect failed', 'Revision abc',
    ])
  })
})

describe('event filters', () => {
  it('matches the text against names, outcomes and details, ignoring case', () => {
    expect(filterEvents([failed, ejected], 'CONNECT failed', ALL_EVENTS)).toEqual([ejected])
    expect(filterEvents([failed, ejected], 'server update', ALL_EVENTS)).toEqual([failed])
    expect(filterEvents([failed, ejected], ' ', ALL_EVENTS)).toEqual([failed, ejected])
  })

  it('combines the event name with the text and lists names once', () => {
    expect(filterEvents([failed, ejected], '', 'server_update')).toEqual([failed])
    expect(filterEvents([failed, ejected], 'ejected', 'server_update')).toEqual([])
    expect(eventNameOptions([failed, failed, ejected]).map(({ label }) => label)).toEqual([
      'All events', 'Server update', 'Upstream endpoint ejection',
    ])
  })

  it('detects a restart only when the reported cursor goes backwards', () => {
    expect(ringRestarted(7, 3)).toBe(true)
    expect(ringRestarted(7, 7)).toBe(false)
    expect(ringRestarted(0, 0)).toBe(false)
  })
})

describe('StreamStatus', () => {
  it.each([
    ['live', 'Live', 'ox-tone-ok'],
    ['connecting', 'Connecting', 'ox-tone-neutral'],
    ['reconnecting', 'Reconnecting', 'ox-tone-warn'],
    ['offline', 'Offline', 'ox-tone-bad'],
  ])('shows %s as the word %s with a shape', (state, word, tone) => {
    const wrapper = mount(StreamStatus, { props: { state } })
    expect(wrapper.text()).toBe(word)
    expect(wrapper.get('.ox-badge').classes()).toContain(tone)
    expect(wrapper.find('svg').exists()).toBe(true)
  })
})
