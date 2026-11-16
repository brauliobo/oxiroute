import type { ConfigSnapshot } from '../../config'

export function formatLabel(format: ConfigSnapshot['configFormat']): string {
  return {
    kdl: 'KDL',
    lua: 'Lua',
    uci: 'UCI',
    hocon: 'HOCON',
  }[format]
}
