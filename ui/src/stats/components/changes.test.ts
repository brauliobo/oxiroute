import { describe, expect, it } from 'vitest'
import { effectScope, nextTick } from 'vue'
import { CHANGE_WINDOW_MS, createStatusChanges, statusesOf } from './changes'
import { documentAt, loadedStore } from './testing'

describe('status changes', () => {
  it('lists the status of every listener, pool and server', async () => {
    const { store } = await loadedStore([documentAt(0)])
    expect(Object.fromEntries(statusesOf(store.model.value!))).toEqual({
      'listener:public-http': 'listening', 'pool:web': 'degraded', 'server:web/origin-a': 'up', 'server:web/origin-b': 'drain',
      'pool:api': 'up', 'server:api/origin-c': 'up',
    })
  })

  it('marks only rows that flipped, and only for a while', async () => {
    const harness = await loadedStore([documentAt(0), documentAt(2, (wire) => {
      wire.pools[0]!.servers[0]!.status = 'down'
      wire.pools[0]!.status = 'down'
    }), documentAt(4, (wire) => { wire.pools[0]!.servers[0]!.status = 'down'; wire.pools[0]!.status = 'down' })])
    const scope = effectScope()
    const changes = scope.run(() => createStatusChanges(harness.store))!
    expect(changes.changed('server:web/origin-a')).toBe(false)
    await harness.next()
    await nextTick()
    expect(['server:web/origin-a', 'pool:web', 'server:web/origin-b', 'pool:api'].map(changes.changed)).toEqual([true, true, false, false])
    await harness.next()
    expect(changes.changed('server:web/origin-a')).toBe(true)
    expect(CHANGE_WINDOW_MS).toBe(10_000)
    scope.stop()
  })
})

