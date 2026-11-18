import type { AdministrativeState, MonitoringListener, MonitoringPool, ServerInventoryEntry } from '../api'
import { formatCount, shortRevision } from '../formatters'
import type { ConfirmRequest } from '../shell/confirm'
import { plural } from './labels'
import type { Snapshot } from './types'

export const SHUTDOWN_PHRASE = 'shutdown'

const STOPS_NEW_WORK: Record<Exclude<AdministrativeState, 'ready'>, string> = {
  drain: 'stops taking new work; what is running finishes',
  maintenance: 'stops taking new work and is marked as under maintenance; what is running finishes',
}

const STATE_TITLES: Record<Exclude<AdministrativeState, 'ready'>, string> = {
  drain: 'Drain',
  maintenance: 'Put in maintenance',
}

export function totalConnections(snapshot: Snapshot): number {
  return (snapshot.listeners?.listeners ?? []).reduce((sum, listener) => sum + listener.activeConnections, 0)
}

export function poolConnections(pool: MonitoringPool): number {
  return pool.endpoints.reduce((sum, endpoint) => sum + Number(endpoint.activeConnections), 0)
}

function connections(count: number): string {
  return `Active connections now: ${formatCount(count)}`
}

export function reloadConfirmation(snapshot: Snapshot): ConfirmRequest {
  return {
    title: 'Reload the persisted configuration',
    message: 'The configuration file on disk is prepared as a new generation and, when it is valid, takes over new ' +
      'traffic. Running requests finish on the generation they started on.',
    confirmLabel: 'Reload',
    details: [
      `Serving now: ${shortRevision(snapshot.generation?.activeRevision ?? null)}`,
      `On disk: ${shortRevision(snapshot.generation?.diskRevision ?? null)}`,
    ],
  }
}

export function rollbackConfirmation(snapshot: Snapshot): ConfirmRequest {
  return {
    title: 'Roll back to the previous generation',
    message: 'The previous generation is activated again and the current one retires after its running requests ' +
      'finish. Changes made since are not served until you reload.',
    confirmLabel: 'Roll back',
    danger: true,
    details: [
      `Serving now: ${shortRevision(snapshot.generation?.activeRevision ?? null)}`,
      `Rolls back to: ${shortRevision(snapshot.generation?.previousRevision ?? null)}`,
    ],
  }
}

export function generationDrainConfirmation(snapshot: Snapshot): ConfirmRequest {
  return {
    title: 'Drain the active generation',
    message: 'The active generation stops admitting new connections on every traffic listener and readiness answers ' +
      '503. Running work is not cancelled. The management listener keeps serving. A reload ends the drain.',
    confirmLabel: 'Drain generation',
    danger: true,
    details: [connections(totalConnections(snapshot)), `Generation: ${shortRevision(snapshot.generation?.activeRevision ?? null)}`],
  }
}

export function processDrainConfirmation(snapshot: Snapshot): ConfirmRequest {
  return {
    title: 'Drain the process',
    message: 'The whole process stops admitting new connections while running work finishes. A reload does not undo ' +
      'it: the process has to be restarted to admit traffic again.',
    confirmLabel: 'Drain process',
    danger: true,
    details: [connections(totalConnections(snapshot))],
  }
}

export function shutdownConfirmation(snapshot: Snapshot): ConfirmRequest {
  return {
    title: 'Shut down the process',
    message: 'The process stops serving and exits within its shutdown deadline. Open connections are closed and this ' +
      'console becomes unreachable until the service is started again. It cannot be undone from here.',
    confirmLabel: 'Shut down',
    danger: true,
    typeToConfirm: SHUTDOWN_PHRASE,
    details: [connections(totalConnections(snapshot))],
  }
}

export function listenerStateConfirmation(
  listener: MonitoringListener, state: Exclude<AdministrativeState, 'ready'>,
): ConfirmRequest {
  return {
    title: `${STATE_TITLES[state]} listener ${listener.name}`,
    message: `The listener ${STOPS_NEW_WORK[state]}. Set it back to Ready to accept connections again.`,
    confirmLabel: STATE_TITLES[state],
    danger: true,
    details: [connections(listener.activeConnections), `Bind: ${listener.bind}`],
  }
}

export function poolStateConfirmation(
  pool: MonitoringPool, state: Exclude<AdministrativeState, 'ready'>,
): ConfirmRequest {
  return {
    title: `${STATE_TITLES[state]} pool ${pool.name}`,
    message: `Every server of the pool ${STOPS_NEW_WORK[state]}. Set the pool back to Ready to hand it back.`,
    confirmLabel: STATE_TITLES[state],
    danger: true,
    details: [plural(pool.endpoints.length, 'server'), connections(poolConnections(pool))],
  }
}

export function serverStateConfirmation(
  entry: ServerInventoryEntry, state: Exclude<AdministrativeState, 'ready'>,
): ConfirmRequest {
  return {
    title: `${STATE_TITLES[state]} server ${entry.server.name}`,
    message: `The server ${STOPS_NEW_WORK[state]}. Set it back to Ready to hand it back.`,
    confirmLabel: STATE_TITLES[state],
    danger: true,
    details: [`Pool: ${entry.pool}`, connections(Number(entry.server.activeConnections))],
  }
}

export function forceDownConfirmation(entry: ServerInventoryEntry): ConfirmRequest {
  return {
    title: `Force server ${entry.server.name} down`,
    message: 'The server is treated as unhealthy whatever its checks say, so no new requests are routed to it. ' +
      'Running requests are not cancelled. Choose Auto health to hand control back to the checks.',
    confirmLabel: 'Force down',
    danger: true,
    details: [`Pool: ${entry.pool}`, connections(Number(entry.server.activeConnections))],
  }
}

const FORCE_EFFECT = 'Force also cuts the requests that cannot move: their callers get a 503, or a closed connection ' +
  'once the response started.'

export function serverEvacuationConfirmation(entry: ServerInventoryEntry, force: boolean): ConfirmRequest {
  return {
    title: `Evacuate server ${entry.server.name}`,
    message: 'The server is drained and its running HTTP requests that can be replayed are cancelled and sent to ' +
      'another server of the pool; requests that cannot move keep running. Set the server back to Ready to hand it ' +
      `back. ${force ? FORCE_EFFECT : 'Without force nothing is cut.'}`,
    confirmLabel: force ? 'Evacuate and force' : 'Evacuate',
    danger: true,
    details: [`Pool: ${entry.pool}`, connections(Number(entry.server.activeConnections)), `Force: ${force ? 'on' : 'off'}`],
  }
}

export function poolEvacuationConfirmation(pool: MonitoringPool, force: boolean): ConfirmRequest {
  return {
    title: `Evacuate pool ${pool.name}`,
    message: 'Every server of the pool is drained first, so no server is left to take a moved request: running ' +
      `requests are reported as having no destination and finish. ${force ? FORCE_EFFECT : 'Without force nothing is cut.'} ` +
      'Set the pool back to Ready to hand it back.',
    confirmLabel: force ? 'Evacuate and force' : 'Evacuate',
    danger: true,
    details: [plural(pool.endpoints.length, 'server'), connections(poolConnections(pool)), `Force: ${force ? 'on' : 'off'}`],
  }
}
