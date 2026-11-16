import type { HealthFailure } from '../../managementContracts'

export interface OperationalEventSimpleOutcomes {
  generation_prepare: 'prepared' | 'rejected' | 'requested' | 'failed'
  generation_activate: 'activated'
  generation_rollback: 'prepared' | 'rejected' | 'requested' | 'failed'
  generation_drain: 'rejected' | 'requested' | 'failed'
  generation_retire: 'retiring' | 'drained' | 'forced_close' | 'cap_reached'
  generation_start: 'quarantined'
  configuration_reload: 'rejected' | 'applied' | 'failed'
  import_completed: 'applied'
  control_operation: 'rejected' | 'requested' | 'failed'
  process_shutdown: 'rejected' | 'requested' | 'failed'
  listener_administrative_state: 'rejected' | 'applied' | 'failed'
  pool_administrative_state: 'rejected' | 'applied' | 'failed'
  server_update: 'rejected' | 'applied' | 'failed'
  rtmp_connect: 'rejected' | 'applied' | 'failed'
  rtmp_publish: 'rejected' | 'applied' | 'failed'
  rtmp_play: 'rejected' | 'applied' | 'failed'
  rtmp_disconnect: 'rejected' | 'applied' | 'failed'
  rtmp_access: 'rejected' | 'applied' | 'failed'
  certificate_renewal: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_activated: 'activated'
  certificate_revocation: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_deletion: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_account_rollover: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_job_control: 'rejected' | 'requested' | 'applied' | 'failed'
  unknown: 'unknown'
}

export type OperationalEventSimpleName = keyof OperationalEventSimpleOutcomes

export type OperationalEventName =
  | OperationalEventSimpleName
  | 'upstream_endpoint_ejection'
  | 'upstream_endpoint_recovery'
  | 'upstream_endpoint_health'
  | 'server_evacuation'

export type OperationalEventSimpleOutcome = OperationalEventSimpleOutcomes[OperationalEventSimpleName]

export interface OperationalEndpointEjectionOutcome {
  type: 'ejected'
  pool: string
  server: string
  reason: HealthFailure
  failureCount: number
  ejectionCount: number
  ejectedAtUnixMs: number
  ejectionUntilUnixMs: number
}

export interface OperationalEndpointRecoveryOutcome {
  type: 'recovered'
  pool: string
  server: string
  reason: HealthFailure | null
  recoveryCount: number
  recoveredAtUnixMs: number
}

export type EndpointHealthStateName = 'unchecked' | 'unknown' | 'healthy' | 'unhealthy'

export interface OperationalEndpointHealthOutcome {
  type: 'health_changed'
  pool: string
  server: string
  previous: EndpointHealthStateName
  next: EndpointHealthStateName
  reason: HealthFailure | null
  changedAtUnixMs: number
}

export interface OperationalEvacuationOutcome {
  type: 'evacuated'
  pool: string
  server?: string
  force: boolean
  cancelledAndRedispatched: number
  forced: number
  leftRunning: Record<string, number>
}

export type OperationalEventOutcome =
  | OperationalEventSimpleOutcome
  | OperationalEndpointEjectionOutcome
  | OperationalEndpointRecoveryOutcome
  | OperationalEndpointHealthOutcome
  | OperationalEvacuationOutcome

interface OperationalEventBase {
  cursor: number
  timestampUnixMs: number | null
  revision: string | null
  certificate?: string
}

export type OperationalEvent =
  | OperationalEventBase & {
    event: 'upstream_endpoint_ejection'
    outcome: OperationalEndpointEjectionOutcome
  }
  | OperationalEventBase & {
    event: 'upstream_endpoint_recovery'
    outcome: OperationalEndpointRecoveryOutcome
  }
  | OperationalEventBase & {
    event: 'upstream_endpoint_health'
    outcome: OperationalEndpointHealthOutcome
  }
  | OperationalEventBase & {
    event: 'server_evacuation'
    outcome: OperationalEvacuationOutcome
  }
  | {
    [Event in OperationalEventSimpleName]: OperationalEventBase & {
      event: Event
      outcome: OperationalEventSimpleOutcomes[Event]
    }
  }[OperationalEventSimpleName]

export interface EventPage {
  events: OperationalEvent[]
  cursor: number
  latestCursor: number
  hasMore: boolean
  oldestCursor: number | null
}
