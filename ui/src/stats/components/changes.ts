import { inject, provide, shallowRef, watch } from 'vue'
import type { InjectionKey } from 'vue'
import type { MeasuredModel } from '../derive'
import type { StatsStore } from '../store'
import { useStatsStore } from '../useStatsStore'

// Rows whose status changed since the previous snapshot stay marked for a few seconds, so a flip is noticed even when
// the eye was elsewhere. The first snapshot marks nothing; a row that appears or disappears is not a flip.

export const CHANGE_WINDOW_MS = 10_000

export interface StatusChanges {
  /** The row (by key) changed status within the last `CHANGE_WINDOW_MS`. */
  changed(key: string): boolean
}
export const STATUS_CHANGES: InjectionKey<StatusChanges> = Symbol('oxiroute-stats-changes')

export const statusesOf = (model: MeasuredModel): Map<string, string> => new Map([
  ...model.listeners.map((row) => [row.key, row.wire.state] as const),
  ...model.pools.flatMap((pool) => [[pool.key, pool.wire.status] as const,
    ...pool.servers.map((server) => [server.key, server.wire.status] as const)]),
])

export function createStatusChanges({ model, nowMs }: Pick<StatsStore, 'model' | 'nowMs'>): StatusChanges {
  const marks = shallowRef(new Map<string, number>())
  let known = new Map<string, string>()
  watch(model, (next) => {
    if (!next) return
    const current = statusesOf(next)
    const flipped = [...current.keys()].filter((key) => known.has(key) && known.get(key) !== current.get(key))
    known = current
    const now = Date.now()
    const live = [...marks.value].filter(([, at]) => now - at < CHANGE_WINDOW_MS)
    marks.value = new Map([...live, ...flipped.map((key) => [key, now] as const)])
  }, { immediate: true })
  return {
    changed(key) {
      const at = marks.value.get(key)
      return at !== undefined && nowMs.value - at < CHANGE_WINDOW_MS
    },
  }
}

/** For `StatsDashboard`: tracks the store's model and provides the result to the tables below it. */
export function provideStatusChanges(): StatusChanges {
  const changes = createStatusChanges(useStatsStore())
  provide(STATUS_CHANGES, changes)
  return changes
}

export function useStatusChanges(): StatusChanges {
  const changes = inject(STATUS_CHANGES)
  if (!changes) throw new Error('useStatusChanges() needs provideStatusChanges() in an ancestor component')
  return changes
}
