import { inject, onScopeDispose, provide } from 'vue'
import type { InjectionKey } from 'vue'
import { createStatsStore } from './store'
import type { StatsStore, StatsStoreOptions } from './store'

export const STATS_STORE: InjectionKey<StatsStore> = Symbol('oxiroute-stats-store')

/** For the root component: creates the store, starts polling, provides it to the tree and stops it with the component. */
export function provideStatsStore(options?: StatsStoreOptions): StatsStore {
  const store = createStatsStore(options)
  provide(STATS_STORE, store)
  onScopeDispose(store.dispose)
  store.start()
  return store
}

/** For every other component: the store the root provided. */
export function useStatsStore(): StatsStore {
  const store = inject(STATS_STORE)
  if (!store) throw new Error('useStatsStore() needs provideStatsStore() in an ancestor component')
  return store
}
