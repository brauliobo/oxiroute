import type { Tone } from '../badge/tone'

export const SERVER_STATUSES = ['up', 'down', 'drain', 'maint', 'ejected', 'unchecked', 'unknown'] as const
export const POOL_STATUSES = ['up', 'degraded', 'down', 'empty'] as const
export const LISTENER_STATUSES = ['configured', 'listening', 'stopped', 'failed'] as const
export const GENERATION_STATUSES = ['active', 'starting', 'degraded'] as const
export const FEED_STATUSES = ['loading', 'live', 'paused', 'stale', 'retrying', 'unavailable'] as const
export const STREAM_STATUSES = ['streaming', 'degraded', 'offline', 'idle'] as const

export type ServerStatus = (typeof SERVER_STATUSES)[number]
export type PoolStatus = (typeof POOL_STATUSES)[number]
export type ListenerStatus = (typeof LISTENER_STATUSES)[number]
export type GenerationStatus = (typeof GENERATION_STATUSES)[number]
export type FeedStatus = (typeof FEED_STATUSES)[number]
export type StreamStatus = (typeof STREAM_STATUSES)[number]
export type Status = ServerStatus | PoolStatus | ListenerStatus | GenerationStatus | FeedStatus | StreamStatus

interface Word {
  label: string
  tone: Tone
}

const WORDS: Readonly<Record<Status, Word>> = {
  up: { label: 'UP', tone: 'ok' },
  down: { label: 'DOWN', tone: 'bad' },
  drain: { label: 'DRAIN', tone: 'warn' },
  maint: { label: 'MAINT', tone: 'info' },
  ejected: { label: 'EJECTED', tone: 'bad' },
  unchecked: { label: 'UNCHECKED', tone: 'neutral' },
  unknown: { label: 'UNKNOWN', tone: 'neutral' },
  degraded: { label: 'DEGRADED', tone: 'warn' },
  empty: { label: 'EMPTY', tone: 'neutral' },
  configured: { label: 'CONFIGURED', tone: 'neutral' },
  listening: { label: 'LISTENING', tone: 'ok' },
  stopped: { label: 'STOPPED', tone: 'warn' },
  failed: { label: 'FAILED', tone: 'bad' },
  active: { label: 'ACTIVE', tone: 'ok' },
  starting: { label: 'STARTING', tone: 'warn' },
  loading: { label: 'Loading', tone: 'neutral' },
  live: { label: 'Live', tone: 'ok' },
  paused: { label: 'Paused', tone: 'info' },
  stale: { label: 'Stale', tone: 'warn' },
  retrying: { label: 'Retrying', tone: 'warn' },
  unavailable: { label: 'Unavailable', tone: 'bad' },
  streaming: { label: 'LIVE', tone: 'ok' },
  offline: { label: 'OFFLINE', tone: 'neutral' },
  idle: { label: 'IDLE', tone: 'info' },
}

export const statusLabel = (status: Status): string => WORDS[status].label
export const toneOf = (status: Status): Tone => WORDS[status].tone

export interface ServerHealth {
  administrativeState: 'ready' | 'drain' | 'maintenance'
  healthOverride: 'auto' | 'up' | 'down'
  state: 'unchecked' | 'unknown' | 'healthy' | 'unhealthy'
  passiveEjected: boolean
}

export function serverStatusOf(server: ServerHealth): ServerStatus {
  if (server.administrativeState === 'maintenance') return 'maint'
  if (server.administrativeState === 'drain') return 'drain'
  if (server.passiveEjected) return 'ejected'
  if (server.healthOverride !== 'auto') return server.healthOverride
  if (server.state === 'healthy') return 'up'
  return server.state === 'unhealthy' ? 'down' : server.state
}

export function poolStatusOf(totalServers: number, availableServers: number): PoolStatus {
  if (totalServers === 0) return 'empty'
  if (availableServers === 0) return 'down'
  return availableServers < totalServers ? 'degraded' : 'up'
}

const RUNTIME_STATES = {
  configured: 'configured',
  listening: 'listening',
  stopped: 'stopped',
  failed: 'failed',
  available: 'up',
  degraded: 'degraded',
  unavailable: 'down',
  unchecked: 'unchecked',
  unknown: 'unknown',
  healthy: 'up',
  unhealthy: 'down',
} as const satisfies Record<string, Status>

export type RuntimeState = keyof typeof RUNTIME_STATES

export const statusOfRuntimeState = (state: RuntimeState): Status => RUNTIME_STATES[state]
