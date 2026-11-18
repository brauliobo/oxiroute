import type { ServerInventoryEntry } from '../api'
import { hasProblem } from './labels'

export function filterServers(
  servers: readonly ServerInventoryEntry[], query: string, problemsOnly: boolean,
): ServerInventoryEntry[] {
  const needle = query.trim().toLowerCase()
  return servers.filter(({ pool, server }) =>
    (!problemsOnly || hasProblem(server)) &&
    (needle === '' || [server.name, pool, server.address, server.origin].some((field) => field.toLowerCase().includes(needle))),
  )
}
