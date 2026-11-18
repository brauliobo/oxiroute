import { describe, expect, it } from 'vitest'

import { canonicalConfig } from '../test/configurationWorkspace'
import { locateDiagnostic } from './diagnosticLocation'

describe('locateDiagnostic', () => {
  const config = canonicalConfig()

  it('resolves list items by name and by index to the same field', () => {
    const byName = locateDiagnostic(config, '/listeners/https/bind')
    expect(byName).toEqual({ selection: 'listeners:0', field: 'listeners[].bind', nested: [] })
    expect(locateDiagnostic(config, '/listeners/0/bind')).toEqual(byName)
    expect(locateDiagnostic(config, '/listeners/database/service'))
      .toEqual({ selection: 'listeners:2', field: 'listeners[].service', nested: [] })
  })

  it('resolves nested named items and keeps their indexes', () => {
    expect(locateDiagnostic(config, '/upstream_pools/web-origins/servers/server-2/endpoint'))
      .toEqual({ selection: 'upstream_pools:0', field: 'upstream_pools[].servers[].endpoint', nested: [1] })
    expect(locateDiagnostic(config, '/http_services/web/routes/0/action/upstream_pool'))
      .toEqual({ selection: 'http_services:0', field: 'http_services[].routes[].action.upstream_pool', nested: [0] })
  })

  it('splits dotted field names the validator appends to a pointer', () => {
    expect(locateDiagnostic(config, '/http_services/web/routes/0/policy.connect_timeout_ms'))
      .toEqual({ selection: 'http_services:0', field: 'http_services[].routes[].policy.connect_timeout_ms', nested: [0] })
  })

  it('drops the leading config segment the request parser adds', () => {
    expect(locateDiagnostic(config, '/config/management/ui_dir'))
      .toEqual({ selection: 'management', field: 'management.ui_dir', nested: [] })
    expect(locateDiagnostic(config, '/config/certificates'))
      .toBeNull()
  })

  it('selects an item without a field when the pointer names only the item', () => {
    expect(locateDiagnostic(config, '/upstream_pools/secure-origins'))
      .toEqual({ selection: 'upstream_pools:1', field: null, nested: [] })
  })

  it('maps singleton sections and general settings', () => {
    expect(locateDiagnostic(config, '/version')).toEqual({ selection: 'general', field: 'version', nested: [] })
    expect(locateDiagnostic(config, '/stats/pages/0/bind'))
      .toEqual({ selection: 'stats', field: 'stats.pages[].bind', nested: [0] })
    expect(locateDiagnostic(config, '/stats/binds/0')).toEqual({ selection: 'stats', field: 'stats.binds', nested: [] })
  })

  it('stops at the deepest list when an item name does not resolve', () => {
    expect(locateDiagnostic(config, '/upstream_pools/web-origins/servers/<pool>/queue'))
      .toEqual({ selection: 'upstream_pools:0', field: 'upstream_pools[].servers', nested: [] })
  })

  it('cannot place the root, unknown items or out-of-range indexes', () => {
    expect(locateDiagnostic(config, '/config')).toBeNull()
    expect(locateDiagnostic(config, '')).toBeNull()
    expect(locateDiagnostic(config, '/listeners')).toBeNull()
    expect(locateDiagnostic(config, '/listeners/ghost/bind')).toBeNull()
    expect(locateDiagnostic(config, '/listeners/99/bind')).toBeNull()
  })

  it('accepts the bracketed index form', () => {
    expect(locateDiagnostic(config, 'listeners[0].bind.address'))
      .toEqual({ selection: 'listeners:0', field: 'listeners[].bind.address', nested: [] })
    expect(locateDiagnostic(config, 'upstream_pools[0].servers[1].endpoint'))
      .toEqual({ selection: 'upstream_pools:0', field: 'upstream_pools[].servers[].endpoint', nested: [1] })
  })

  it('decodes escaped pointer segments', () => {
    const named = { ...config, listeners: [{ ...config.listeners[0]!, name: 'a/b~c' }] }
    expect(locateDiagnostic(named, '/listeners/a~1b~0c/bind'))
      .toEqual({ selection: 'listeners:0', field: 'listeners[].bind', nested: [] })
  })
})
