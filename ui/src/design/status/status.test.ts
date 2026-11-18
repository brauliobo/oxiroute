import { describe, expect, it } from 'vitest'
import { ICON_PATHS } from '../badge/tone'
import { mount } from '../../test/mount'
import StatusBadge from './StatusBadge.vue'
import {
  FEED_STATUSES, GENERATION_STATUSES, LISTENER_STATUSES, POOL_STATUSES, SERVER_STATUSES, poolStatusOf, serverStatusOf,
  statusLabel, statusOfRuntimeState, toneOf,
} from './status'
import type { RuntimeState, ServerHealth } from './status'

const healthy: ServerHealth = { administrativeState: 'ready', healthOverride: 'auto', state: 'healthy', passiveEjected: false }

describe('status words', () => {
  it('names every server state with the words of the public stats page', () => {
    expect(SERVER_STATUSES.map(statusLabel)).toEqual(['UP', 'DOWN', 'DRAIN', 'MAINT', 'EJECTED', 'UNCHECKED', 'UNKNOWN'])
    expect(SERVER_STATUSES.map(toneOf)).toEqual(['ok', 'bad', 'warn', 'info', 'bad', 'neutral', 'neutral'])
  })

  it('names every pool state', () => {
    expect(POOL_STATUSES.map(statusLabel)).toEqual(['UP', 'DEGRADED', 'DOWN', 'EMPTY'])
    expect(POOL_STATUSES.map(toneOf)).toEqual(['ok', 'warn', 'bad', 'neutral'])
  })

  it('names every listener and generation state', () => {
    expect(LISTENER_STATUSES.map(statusLabel)).toEqual(['CONFIGURED', 'LISTENING', 'STOPPED', 'FAILED'])
    expect(LISTENER_STATUSES.map(toneOf)).toEqual(['neutral', 'ok', 'warn', 'bad'])
    expect(GENERATION_STATUSES.map(statusLabel)).toEqual(['ACTIVE', 'STARTING', 'DEGRADED'])
    expect(GENERATION_STATUSES.map(toneOf)).toEqual(['ok', 'warn', 'warn'])
  })

  it('names every feed state', () => {
    expect(FEED_STATUSES.map(statusLabel)).toEqual(['Loading', 'Live', 'Paused', 'Stale', 'Retrying', 'Unavailable'])
    expect(FEED_STATUSES.map(toneOf)).toEqual(['neutral', 'ok', 'info', 'warn', 'warn', 'bad'])
  })

  it('never gives two different states the same word', () => {
    const all = [...SERVER_STATUSES, ...POOL_STATUSES, ...LISTENER_STATUSES, ...GENERATION_STATUSES, ...FEED_STATUSES]
    const words = new Set(all.map(statusLabel))
    expect(words.size).toBe(new Set(all).size)
  })
})

describe('serverStatusOf', () => {
  it('lets the administrative state win over ejection, and ejection over health', () => {
    expect(serverStatusOf({ ...healthy, administrativeState: 'maintenance', passiveEjected: true })).toBe('maint')
    expect(serverStatusOf({ ...healthy, administrativeState: 'drain', passiveEjected: true })).toBe('drain')
    expect(serverStatusOf({ ...healthy, passiveEjected: true })).toBe('ejected')
  })

  it('lets a forced health override win over the checks', () => {
    expect(serverStatusOf({ ...healthy, healthOverride: 'down' })).toBe('down')
    expect(serverStatusOf({ ...healthy, healthOverride: 'up', state: 'unhealthy' })).toBe('up')
  })

  it.each([
    ['healthy', 'up'],
    ['unhealthy', 'down'],
    ['unchecked', 'unchecked'],
    ['unknown', 'unknown'],
  ] as const)('reads the %s check result as %s', (state, expected) => {
    expect(serverStatusOf({ ...healthy, state })).toBe(expected)
  })
})

describe('poolStatusOf', () => {
  it('reads the available share of the servers', () => {
    expect(poolStatusOf(0, 0)).toBe('empty')
    expect(poolStatusOf(3, 0)).toBe('down')
    expect(poolStatusOf(3, 2)).toBe('degraded')
    expect(poolStatusOf(3, 3)).toBe('up')
  })
})

describe('statusOfRuntimeState', () => {
  it.each<[RuntimeState, string]>([
    ['available', 'UP'], ['degraded', 'DEGRADED'], ['unavailable', 'DOWN'], ['healthy', 'UP'], ['unhealthy', 'DOWN'],
    ['unchecked', 'UNCHECKED'], ['unknown', 'UNKNOWN'], ['listening', 'LISTENING'], ['failed', 'FAILED'],
    ['configured', 'CONFIGURED'], ['stopped', 'STOPPED'],
  ])('shows the runtime state %s as %s', (state, word) => {
    expect(statusLabel(statusOfRuntimeState(state))).toBe(word)
  })
})

describe('StatusBadge', () => {
  it('shows the shared word and the shape of its tone', () => {
    const wrapper = mount(StatusBadge, { props: { status: 'ejected' } })
    expect(wrapper.text()).toBe('EJECTED')
    expect(wrapper.classes()).toContain('ox-tone-bad')
    expect(wrapper.get('path').attributes('d')).toBe(ICON_PATHS.bad)
  })
})
