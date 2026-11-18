import type { Tone } from '#design/badge/tone'
import type {
  AdministrativeState,
  EndpointHealthState,
  ListenerRuntimeState,
  MonitoringPool,
  MonitoringPoolEndpoint,
} from '../api'
import { formatCount } from '../formatters'

export interface StateBadge {
  label: string
  tone: Tone
}

export const ADMIN_BADGES: Record<AdministrativeState, StateBadge> = {
  ready: { label: 'Ready', tone: 'ok' },
  drain: { label: 'Draining', tone: 'warn' },
  maintenance: { label: 'Maintenance', tone: 'info' },
}

export const HEALTH_BADGES: Record<EndpointHealthState, StateBadge> = {
  healthy: { label: 'Healthy', tone: 'ok' },
  unhealthy: { label: 'Unhealthy', tone: 'bad' },
  unknown: { label: 'Unknown', tone: 'warn' },
  unchecked: { label: 'Unchecked', tone: 'neutral' },
}

export const LISTENER_BADGES: Record<ListenerRuntimeState, StateBadge> = {
  listening: { label: 'Listening', tone: 'ok' },
  configured: { label: 'Configured', tone: 'info' },
  stopped: { label: 'Stopped', tone: 'warn' },
  failed: { label: 'Failed', tone: 'bad' },
}

export const ADMIN_STATES: readonly AdministrativeState[] = ['ready', 'drain', 'maintenance']

export function uniformAdminState(pool: MonitoringPool): AdministrativeState | null {
  const [first] = pool.endpoints
  const uniform = first !== undefined && pool.endpoints.every((endpoint) => endpoint.administrativeState === first.administrativeState)
  return uniform ? first.administrativeState : null
}

export function poolAdminBadge(pool: MonitoringPool): StateBadge {
  const total = pool.endpoints.length
  if (total === 0) return { label: 'No servers', tone: 'neutral' }
  const uniform = uniformAdminState(pool)
  if (uniform) return ADMIN_BADGES[uniform]
  const waiting = pool.endpoints.filter((endpoint) => endpoint.administrativeState !== 'ready').length
  return { label: `Mixed: ${waiting} of ${total} not ready`, tone: 'warn' }
}

export function isDnsAddress(address: string): boolean {
  if (address.startsWith('/') || address.startsWith('[')) return false
  const host = address.slice(0, address.lastIndexOf(':'))
  return !/^\d{1,3}(\.\d{1,3}){3}$/.test(host)
}

export function checksApplicable(server: MonitoringPoolEndpoint): boolean {
  return server.state !== 'unchecked' || !server.checksEnabled
}

export function hasProblem(server: MonitoringPoolEndpoint): boolean {
  return server.state === 'unhealthy' || server.passiveEjected || server.healthOverride === 'down' ||
    server.administrativeState !== 'ready'
}

export function humanize(value: string): string {
  return value.replaceAll('_', ' ')
}

export function plural(count: number, noun: string): string {
  return `${formatCount(count)} ${noun}${count === 1 ? '' : 's'}`
}

export function limitText(limit: number | null): string {
  return limit === null ? 'unbounded' : formatCount(limit)
}
