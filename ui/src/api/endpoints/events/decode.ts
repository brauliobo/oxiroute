import { isRecord, safeInteger } from '../../../valueGuards'
import type { HealthFailure } from '../../managementContracts'
import { invalidPayload, isCountMap } from '../shared'
import type {
  EndpointHealthStateName,
  EventPage,
  OperationalEvent,
  OperationalEventName,
  OperationalEventOutcome,
  OperationalEventSimpleName,
  OperationalEventSimpleOutcome,
  OperationalEventSimpleOutcomes,
} from './types'

const OPERATIONAL_EVENT_NAMES: readonly OperationalEventName[] = [
  'generation_prepare',
  'generation_activate',
  'generation_rollback',
  'generation_drain',
  'generation_retire',
  'generation_start',
  'configuration_reload',
  'import_completed',
  'control_operation',
  'process_shutdown',
  'listener_administrative_state',
  'pool_administrative_state',
  'server_update',
  'rtmp_connect',
  'rtmp_publish',
  'rtmp_play',
  'rtmp_disconnect',
  'rtmp_access',
  'certificate_renewal',
  'certificate_activated',
  'certificate_revocation',
  'certificate_deletion',
  'certificate_account_rollover',
  'certificate_job_control',
  'upstream_endpoint_ejection',
  'upstream_endpoint_recovery',
  'upstream_endpoint_health',
  'server_evacuation',
  'unknown',
]
const OPERATIONAL_EVENT_OUTCOMES: {
  [Event in OperationalEventSimpleName]: readonly OperationalEventSimpleOutcomes[Event][]
} = {
  generation_prepare: ['prepared', 'rejected', 'requested', 'failed'],
  generation_activate: ['activated'],
  generation_rollback: ['prepared', 'rejected', 'requested', 'failed'],
  generation_drain: ['rejected', 'requested', 'failed'],
  generation_retire: ['retiring', 'drained', 'forced_close', 'cap_reached'],
  generation_start: ['quarantined'],
  configuration_reload: ['rejected', 'applied', 'failed'],
  import_completed: ['applied'],
  control_operation: ['rejected', 'requested', 'failed'],
  process_shutdown: ['rejected', 'requested', 'failed'],
  listener_administrative_state: ['rejected', 'applied', 'failed'],
  pool_administrative_state: ['rejected', 'applied', 'failed'],
  server_update: ['rejected', 'applied', 'failed'],
  rtmp_connect: ['rejected', 'applied', 'failed'],
  rtmp_publish: ['rejected', 'applied', 'failed'],
  rtmp_play: ['rejected', 'applied', 'failed'],
  rtmp_disconnect: ['rejected', 'applied', 'failed'],
  rtmp_access: ['rejected', 'applied', 'failed'],
  certificate_renewal: ['rejected', 'requested', 'applied', 'failed'],
  certificate_activated: ['activated'],
  certificate_revocation: ['rejected', 'requested', 'applied', 'failed'],
  certificate_deletion: ['rejected', 'requested', 'applied', 'failed'],
  certificate_account_rollover: ['rejected', 'requested', 'applied', 'failed'],
  certificate_job_control: ['rejected', 'requested', 'applied', 'failed'],
  unknown: ['unknown'],
}

export function parseOperationalEvent(
  value: Record<string, unknown>,
  expectedName?: OperationalEventName,
  expectedCursor?: number | null,
  expectedId?: number | null,
): OperationalEvent | null {
  const cursor = eventCursor(value, 'cursor')
  const timestampUnixMs = value.timestampUnixMs === null
    ? null
    : safeInteger(value.timestampUnixMs)
      ? value.timestampUnixMs
      : undefined
  const eventName = value.event === 'certificate_activation'
    ? 'certificate_activated'
    : value.event
  const event = isOperationalEventName(eventName) ? eventName : null
  const outcome = event === null ? null : parseOperationalEventOutcome(event, value.outcome)
  const revision = value.revision === null
    ? null
    : typeof value.revision === 'string'
      ? value.revision
      : undefined
  const certificate = value.certificate === undefined
    ? undefined
    : typeof value.certificate === 'string'
      ? value.certificate
      : null
  if (cursor === null || (expectedCursor !== undefined && cursor !== expectedCursor) ||
    (expectedId !== undefined && expectedId !== cursor) || timestampUnixMs === undefined ||
    event === null || (expectedName !== undefined && event !== expectedName) || outcome === null ||
    revision === undefined || certificate === null) return null
  const common = {
    cursor,
    timestampUnixMs,
    revision,
    ...(certificate === undefined ? {} : { certificate }),
  }
  if (event === 'upstream_endpoint_ejection') {
    return typeof outcome !== 'string' && outcome.type === 'ejected'
      ? { ...common, event, outcome }
      : null
  }
  if (event === 'upstream_endpoint_recovery') {
    return typeof outcome !== 'string' && outcome.type === 'recovered'
      ? { ...common, event, outcome }
      : null
  }
  if (event === 'upstream_endpoint_health') {
    return typeof outcome !== 'string' && outcome.type === 'health_changed'
      ? { ...common, event, outcome }
      : null
  }
  if (event === 'server_evacuation') {
    return typeof outcome !== 'string' && outcome.type === 'evacuated'
      ? { ...common, event, outcome }
      : null
  }
  return typeof outcome === 'string' ? { ...common, event, outcome } as OperationalEvent : null
}

export function eventCursor(value: unknown, key: string): number | null {
  if (!isRecord(value)) return null
  return parseEventCursor(value[key])
}

export function parseEventCursor(value: unknown): number | null {
  if (safeInteger(value)) return value
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) return null
  const cursor = Number(value)
  return Number.isSafeInteger(cursor) && cursor >= 0 ? cursor : null
}

export function isOperationalEventName(value: unknown): value is OperationalEventName {
  return typeof value === 'string' && OPERATIONAL_EVENT_NAMES.includes(value as OperationalEventName)
}

function parseOperationalEventOutcome(
  event: OperationalEventName,
  value: unknown,
): OperationalEventOutcome | null {
  if (event === 'upstream_endpoint_ejection' && isRecord(value) &&
    typeof value.pool === 'string' && typeof value.server === 'string' &&
    value.type === 'ejected' && isHealthFailure(value.reason) && safeInteger(value.failureCount) &&
    safeInteger(value.ejectionCount) && safeInteger(value.ejectedAtUnixMs) &&
    safeInteger(value.ejectionUntilUnixMs)
  ) {
    return {
      type: 'ejected',
      pool: value.pool,
      server: value.server,
      reason: value.reason,
      failureCount: value.failureCount,
      ejectionCount: value.ejectionCount,
      ejectedAtUnixMs: value.ejectedAtUnixMs,
      ejectionUntilUnixMs: value.ejectionUntilUnixMs,
    }
  }
  if (event === 'upstream_endpoint_recovery' && isRecord(value) &&
    typeof value.pool === 'string' && typeof value.server === 'string' &&
    value.type === 'recovered' && (value.reason === null || isHealthFailure(value.reason)) &&
    safeInteger(value.recoveryCount) && safeInteger(value.recoveredAtUnixMs)
  ) {
    return {
      type: 'recovered',
      pool: value.pool,
      server: value.server,
      reason: value.reason,
      recoveryCount: value.recoveryCount,
      recoveredAtUnixMs: value.recoveredAtUnixMs,
    }
  }
  if (event === 'upstream_endpoint_health' && isRecord(value) &&
    typeof value.pool === 'string' && typeof value.server === 'string' &&
    value.type === 'health_changed' && isEndpointHealthState(value.previous) &&
    isEndpointHealthState(value.next) && (value.reason === null || isHealthFailure(value.reason)) &&
    safeInteger(value.changedAtUnixMs)
  ) {
    return {
      type: 'health_changed',
      pool: value.pool,
      server: value.server,
      previous: value.previous,
      next: value.next,
      reason: value.reason,
      changedAtUnixMs: value.changedAtUnixMs,
    }
  }
  if (event === 'server_evacuation' && isRecord(value) && value.type === 'evacuated' &&
    typeof value.pool === 'string' && (value.server === undefined || typeof value.server === 'string') &&
    typeof value.force === 'boolean' && safeInteger(value.cancelledAndRedispatched) &&
    safeInteger(value.forced) && isCountMap(value.leftRunning)
  ) {
    return {
      type: 'evacuated',
      pool: value.pool,
      ...(value.server === undefined ? {} : { server: value.server }),
      force: value.force,
      cancelledAndRedispatched: value.cancelledAndRedispatched,
      forced: value.forced,
      leftRunning: value.leftRunning,
    }
  }
  if (event === 'upstream_endpoint_ejection' || event === 'upstream_endpoint_recovery' ||
    event === 'upstream_endpoint_health' || event === 'server_evacuation' || typeof value !== 'string'
  ) return null
  return (OPERATIONAL_EVENT_OUTCOMES[event] as readonly string[]).includes(value)
    ? value as OperationalEventSimpleOutcome
    : null
}

function isEndpointHealthState(value: unknown): value is EndpointHealthStateName {
  return typeof value === 'string' && ['unchecked', 'unknown', 'healthy', 'unhealthy'].includes(value)
}

function isHealthFailure(value: unknown): value is HealthFailure {
  return typeof value === 'string' &&
    ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error', 'slow'].includes(value)
}

export function parseEventPage(value: unknown, requireLatestCursor: boolean): EventPage {
  const latestCursor = isRecord(value) && value.latestCursor === undefined && !requireLatestCursor
    ? value.cursor
    : isRecord(value)
      ? value.latestCursor
      : undefined
  if (!isRecord(value) || !Array.isArray(value.events) ||
    !value.events.every((event) => isRecord(event) && parseOperationalEvent(event) !== null) ||
    !safeInteger(value.cursor) || !safeInteger(latestCursor) || typeof value.hasMore !== 'boolean' ||
    !(value.oldestCursor === null || safeInteger(value.oldestCursor))
  ) return invalidPayload('event history')
  const events = value.events.map((event) => parseOperationalEvent(event as Record<string, unknown>))
  if (events.some((event): event is null => event === null)) return invalidPayload('event history')
  return {
    events: events as OperationalEvent[],
    cursor: value.cursor,
    latestCursor,
    hasMore: value.hasMore,
    oldestCursor: value.oldestCursor,
  }
}
