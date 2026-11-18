import { describe, expect, it } from 'vitest'
import { managementGeneration, managementListeners, managementStatus } from '../test/managementFixtures'
import { operationsInventory } from '../test/operationsFixtures'
import * as confirmations from './confirmations'
import { checksApplicable, hasProblem, isDnsAddress, poolAdminBadge } from './labels'
import * as plans from './plans'
import { filterServers } from './serverFilter'
import type { Snapshot } from './types'
import { draftFilter, emptyDraft } from './useInflightRequests'

const { pools, servers } = operationsInventory()
const snapshot: Snapshot = {
  status: managementStatus(),
  generation: managementGeneration().generation,
  listeners: managementListeners(),
  pools,
  servers,
}
const primary = servers.servers[0]!
const web = pools.pools[0]!

describe('labels', () => {
  it.each([
    ['127.0.0.1:3000', false],
    ['[::1]:80', false],
    ['/run/app.sock', false],
    ['origin.example.com:8080', true],
    ['localhost:9000', true],
  ])('DNS address %s is %s', (address, dns) => expect(isDnsAddress(address)).toBe(dns))

  it('applies checks to a checked server or one whose checks were switched off', () => {
    const unchecked = servers.servers[2]!.server
    expect(checksApplicable(unchecked)).toBe(false)
    expect(checksApplicable({ ...unchecked, checksEnabled: false })).toBe(true)
    expect(checksApplicable(primary.server)).toBe(true)
  })

  it('flags unhealthy, ejected, forced-down and not-ready servers as problems', () => {
    expect(hasProblem(primary.server)).toBe(false)
    expect(hasProblem({ ...primary.server, state: 'unhealthy' })).toBe(true)
    expect(hasProblem({ ...primary.server, passiveEjected: true })).toBe(true)
    expect(hasProblem({ ...primary.server, healthOverride: 'down' })).toBe(true)
    expect(hasProblem({ ...primary.server, administrativeState: 'maintenance' })).toBe(true)
  })

  it('derives the administrative state of a pool from its servers', () => {
    expect(poolAdminBadge(web).label).toBe('Mixed: 1 of 2 not ready')
    expect(poolAdminBadge({ ...web, endpoints: [] }).label).toBe('No servers')
    expect(poolAdminBadge({ ...web, endpoints: web.endpoints.map((e) => ({ ...e, administrativeState: 'drain' })) }).label)
      .toBe('Draining')
  })
})

describe('serverFilter', () => {
  it('matches name, pool, address and origin case-insensitively', () => {
    const names = (query: string, problems = false) => filterServers(servers.servers, query, problems).map(({ server }) => server.name)
    expect(names('')).toEqual(['primary', 'secondary', 'static-1'])
    expect(names('WEB-back')).toEqual(['primary', 'secondary'])
    expect(names('3001')).toEqual(['secondary'])
    expect(names('', true)).toEqual(['secondary'])
    expect(names('primary', true)).toEqual([])
  })
})

describe('confirmations', () => {
  it('state what stops and how many connections are affected', () => {
    const drain = confirmations.serverStateConfirmation(primary, 'drain')
    expect(drain.title).toBe('Drain server primary')
    expect(drain.message).toContain('stops taking new work')
    expect(drain.details).toContain('Active connections now: 3')
    expect(confirmations.listenerStateConfirmation(snapshot.listeners!.listeners[0]!, 'maintenance').title)
      .toBe('Put in maintenance listener HTTP ingress')
    expect(confirmations.poolStateConfirmation(web, 'drain').details).toContain('2 servers')
  })

  it('sums the active connections of every listener for the process actions', () => {
    expect(confirmations.totalConnections(snapshot)).toBe(42)
    expect(confirmations.shutdownConfirmation(snapshot).typeToConfirm).toBe(confirmations.SHUTDOWN_PHRASE)
  })

  it('say whether force cuts what cannot move', () => {
    expect(confirmations.serverEvacuationConfirmation(primary, true).message).toContain('Force also cuts')
    expect(confirmations.serverEvacuationConfirmation(primary, false).message).toContain('nothing is cut')
    expect(confirmations.poolEvacuationConfirmation(web, false).details).toContain('Force: off')
  })
})

describe('plans', () => {
  it('ask for nothing when restoring and for everything when taking out of service', () => {
    expect(plans.serverState(primary, 'ready').confirm).toBeUndefined()
    expect(plans.serverState(primary, 'drain').confirm).toBeDefined()
    expect(plans.serverHealth(primary, 'up').confirm).toBeUndefined()
    expect(plans.serverHealth(primary, 'down').confirm).toBeDefined()
    expect(plans.listenerState(snapshot.listeners!.listeners[0]!, 'ready').confirm).toBeUndefined()
  })

  it('report the persistence of runtime state', () => {
    const success = plans.serverWeight(primary, 3).succeeded({ outcome: 'applied', changed: 1 })
    expect(success.tone).toBe('ok')
    expect(success.details).toContain(plans.RUNTIME_STATE_PERSISTENCE)
  })

  it('report an evacuation with the requests left running by reason', () => {
    const success = plans.evacuateServer(primary, false).succeeded({
      outcome: 'evacuated', force: false, cancelledAndRedispatched: 1, forced: 0, leftRunning: { response_started: 2, upgraded: 0 },
    })
    expect(success.tone).toBe('warn')
    expect(success.summary).toContain('1 request moved')
    expect(success.details).toContain('2 requests left running: the backend already started its response.')
    expect(success.details?.join(' ')).not.toContain('upgraded')
  })

  it('report a clean evacuation as done', () => {
    const success = plans.evacuatePool(web, false).succeeded({
      outcome: 'evacuated', force: false, cancelledAndRedispatched: 0, forced: 0, leftRunning: {},
    })
    expect(success.tone).toBe('ok')
  })

  it('report a drain that is still running and a restart that is still needed', () => {
    const drain = plans.drainActiveGeneration(snapshot).succeeded({ outcome: 'draining', activeReferences: 4 })
    expect(drain).toMatchObject({ tone: 'warn', summary: expect.stringContaining('4 connections still open') })

    const reload = plans.reload(snapshot).succeeded({
      outcome: 'startup_requested',
      candidateRevision: 'next',
      restartRequired: { required: true, candidateRevision: 'next', since: 1, reasons: [{ code: 'c', path: '/p', summary: 's' }] },
    })
    expect(reload.tone).toBe('warn')
    expect(reload.details).toEqual(['/p: s'])
  })

  it('report a DNS refresh failure', () => {
    const dns = plans.serverDns(servers.servers[2]!).succeeded({
      outcome: 'partially_refreshed', atomic: false,
      servers: [{ pool: 'static', server: 'static-1', outcome: 'failed', errorCode: 'dns_refresh_failed' }],
    })
    expect(dns.tone).toBe('warn')
    expect(dns.summary).toContain('dns_refresh_failed')
  })
})

describe('in-flight filter', () => {
  it('maps the draft to the API filter and drops empty fields', () => {
    expect(draftFilter(emptyDraft())).toEqual({
      pool: undefined, route: undefined, phase: undefined, minAgeMs: undefined, limit: 100, requestId: undefined,
    })
    expect(draftFilter({ pool: ' web ', route: 'api', phase: 'queued', minAgeSeconds: '1.5', limit: '10', requestId: 'rq' }))
      .toEqual({ pool: 'web', route: 'api', phase: 'queued', minAgeMs: 1500, limit: 10, requestId: 'rq' })
  })

  it.each([['0'], ['1001'], ['2.5'], ['x']])('refuses the limit %s', (limit) => {
    expect(draftFilter({ ...emptyDraft(), limit })).toContain('from 1 to 1000')
  })

  it('refuses a negative age', () => {
    expect(draftFilter({ ...emptyDraft(), minAgeSeconds: '-1' })).toContain('zero or more seconds')
  })
})
