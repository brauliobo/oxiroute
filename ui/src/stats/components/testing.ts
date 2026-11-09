import { afterEach } from 'vitest'
import { createComponent, defineVaporComponent, provide } from 'vue'
import type { Component } from 'vue'
import { mount } from '../../test/mount'
import { decodeSnapshot } from '../decode'
import type { StatsSnapshot } from '../decode'
import { wireDocument } from '../fixtures'
import type { StatsDocument } from '../generated/statsPage'
import { createStatsStore } from '../store'
import type { StatsStore } from '../store'
import { STATS_STORE } from '../useStatsStore'
import { STATUS_CHANGES, createStatusChanges } from './changes'

// Test harness for the data views: a real store fed by documents in contract shape, and a mount that provides it
// (and the status changes) the way StatsDashboard does.

export const BASE = wireDocument().sampledAtUnixMs

/** The base document `seconds` after the first sample; `edit` changes it in place. */
export function documentAt(seconds: number, edit: (wire: StatsDocument) => void = () => {}): StatsDocument {
  const wire = wireDocument({ sampledAtUnixMs: BASE + seconds * 1000 })
  edit(wire)
  return wire
}

export interface Harness { store: StatsStore; next(): Promise<void> }

const stores = new Set<StatsStore>()
afterEach(() => { stores.forEach((store) => store.dispose()); stores.clear() })

/** A started store that loads the documents one by one: the first now, each further one on `next()`. */
export async function loadedStore(documents: StatsDocument[], search = ''): Promise<Harness> {
  const queue = documents.map((wire): StatsSnapshot => decodeSnapshot(JSON.parse(JSON.stringify(wire))))
  const location = { pathname: '/stats', search, hash: '' }
  const history = { state: null, replaceState: (_s: unknown, _t: string, url: string) => {
    location.search = url.includes('?') ? url.slice(url.indexOf('?')) : ''
  } }
  const store = createStatsStore({
    load: async () => (queue.length > 1 ? queue.shift()! : queue[0]!), location: { location, history }, tickMs: 60_000,
  })
  stores.add(store)
  store.start()
  const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0))
  await settle()
  return { store, next: async () => { store.refresh(); await settle() } }
}

/** A started store whose first snapshot never arrives. */
export function waitingStore(): Harness {
  const location = { pathname: '/stats', search: '', hash: '' }
  const history = { state: null, replaceState: () => {} }
  const store = createStatsStore({ load: () => new Promise(() => {}), location: { location, history }, tickMs: 60_000 })
  stores.add(store)
  store.start()
  return { store, next: async () => {} }
}

export function mountView(component: Component, harness: Harness, props: Record<string, unknown> = {}) {
  const Host = defineVaporComponent(() => {
    provide(STATS_STORE, harness.store)
    provide(STATUS_CHANGES, createStatusChanges(harness.store))
    return createComponent(component as never, { $: [() => props] })
  })
  return mount(Host)
}
