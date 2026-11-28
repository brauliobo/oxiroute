import {
  drainGeneration,
  drainProcess,
  evacuatePools,
  evacuateServers,
  refreshServerDns,
  reloadGeneration,
  rollbackGeneration,
  setListenerAdministrativeState,
  setPoolAdministrativeState,
  setServerAdministrativeState,
  setServerChecks,
  setServerHealthOverride,
  setServerMaxConnections,
  setServerWeight,
  shutdownProcess,
  type AdministrativeState,
  type DnsRefreshResponse,
  type EvacuationResponse,
  type HealthOverride,
  type MonitoringListener,
  type MonitoringPool,
  type MutationResponse,
  type ServerInventoryEntry,
} from '../api'
import { shortRevision } from '../formatters'
import * as confirmations from './confirmations'
import { ADMIN_BADGES, plural } from './labels'
import type { ActionPlan, Snapshot, Success } from './types'

export const RUNTIME_STATE_PERSISTENCE =
  'Runtime state only: kept across reloads and rollbacks, not written to the configuration file, reset by a restart.'
export const OVERLAY_STATE_PERSISTENCE =
  'Saved to the sidecar persistence overlay (.overlay.json): survives restarts without editing the configuration file. Promote or clear it in the Overlays tab.'

const LEFT_RUNNING_REASONS: Record<string, string> = {
  response_started: 'the backend already started its response',
  not_replayable: 'the route policy does not allow a replay',
  body_not_replayable: 'the request body outgrew the retry buffer',
  upgraded: 'upgraded connections such as WebSocket',
  limit_reached: 'already evacuated three times',
  no_destination: 'no other server can take them now',
}

type Mutable = Exclude<AdministrativeState, 'ready'>

const target = (entry: ServerInventoryEntry) => [{ pool: entry.pool, server: entry.server.name }]
const serverLabel = (entry: ServerInventoryEntry) => `server ${entry.server.name} in pool ${entry.pool}`
const runtimeState = (summary: string, persisted = false): Success => ({
  tone: 'ok',
  summary,
  details: [persisted ? OVERLAY_STATE_PERSISTENCE : RUNTIME_STATE_PERSISTENCE],
})

function evacuationSuccess(subject: string, report: EvacuationResponse): Success {
  const left = Object.entries(report.leftRunning).filter(([, count]) => count > 0)
  const leftTotal = left.reduce((sum, [, count]) => sum + count, 0)
  return {
    tone: leftTotal > 0 ? 'warn' : 'ok',
    summary: `Evacuated ${subject}: ${plural(report.cancelledAndRedispatched, 'request')} moved to other servers, ` +
      `${plural(report.forced, 'request')} cut, ${plural(leftTotal, 'request')} left running.`,
    details: [
      ...left.map(([reason, count]) => `${plural(count, 'request')} left running: ${LEFT_RUNNING_REASONS[reason] ?? reason}.`),
      RUNTIME_STATE_PERSISTENCE,
    ],
  }
}

function dnsSuccess(entry: ServerInventoryEntry, response: DnsRefreshResponse): Success {
  const result = response.servers[0]
  const failed = response.servers.filter((server) => server.outcome === 'failed').length
  if (failed > 0) {
    return { tone: 'warn', summary: `DNS refresh of ${serverLabel(entry)} failed${result?.errorCode ? ` (${result.errorCode})` : ''}.` }
  }
  return { tone: 'ok', summary: `Refreshed DNS of ${serverLabel(entry)}: ${(result?.addresses ?? []).join(', ') || 'no addresses'}.` }
}

function reloadSuccess(response: MutationResponse, subject: string): Success {
  const restart = response.restartRequired
  const revision = shortRevision(response.candidateRevision ?? null)
  if (!restart?.required) return { tone: 'ok', summary: `${subject} requested: candidate revision ${revision}.` }
  return {
    tone: 'warn',
    summary: `${subject} requested: candidate revision ${revision}, but a restart is still required.`,
    details: restart.reasons.map((reason) => `${reason.path}: ${reason.summary}`),
  }
}

export function reload(snapshot: Snapshot): ActionPlan<MutationResponse> {
  return {
    scope: 'generation',
    failure: 'reload the persisted configuration',
    confirm: confirmations.reloadConfirmation(snapshot),
    run: reloadGeneration,
    succeeded: (response) => reloadSuccess(response, 'Reload'),
  }
}

export function rollback(snapshot: Snapshot): ActionPlan<MutationResponse> {
  return {
    scope: 'generation',
    failure: 'roll back to the previous generation',
    confirm: confirmations.rollbackConfirmation(snapshot),
    run: rollbackGeneration,
    succeeded: (response) => reloadSuccess(response, 'Rollback'),
  }
}

export function drainActiveGeneration(snapshot: Snapshot): ActionPlan<MutationResponse> {
  return {
    scope: 'generation',
    failure: 'drain the active generation',
    confirm: confirmations.generationDrainConfirmation(snapshot),
    run: (revision, token) => drainGeneration(revision, token),
    succeeded: (response) => response.outcome === 'drained'
      ? { tone: 'ok', summary: 'The active generation is drained: no traffic connection remains.' }
      : {
        tone: 'warn',
        summary: `The active generation is draining: ${plural(response.activeReferences ?? 0, 'connection')} still open.`,
      },
  }
}

export function drainRunningProcess(snapshot: Snapshot): ActionPlan<MutationResponse> {
  return {
    scope: 'generation',
    failure: 'drain the process',
    confirm: confirmations.processDrainConfirmation(snapshot),
    run: drainProcess,
    succeeded: () => ({ tone: 'warn', summary: 'The process is draining and admits no new connections.' }),
  }
}

export function shutdown(snapshot: Snapshot): ActionPlan<MutationResponse> {
  return {
    scope: 'generation',
    failure: 'request the process shutdown',
    confirm: confirmations.shutdownConfirmation(snapshot),
    run: shutdownProcess,
    succeeded: () => ({ tone: 'warn', summary: 'Shutdown requested. The process may stop serving this page.' }),
  }
}

export function listenerState(listener: MonitoringListener, state: AdministrativeState, persistOverlay = false): ActionPlan<MutationResponse> {
  const persist = persistOverlay && state !== 'drain'
  return {
    scope: 'admission',
    failure: `set listener ${listener.name} to ${ADMIN_BADGES[state].label.toLowerCase()}`,
    confirm: state === 'ready' ? undefined : confirmations.listenerStateConfirmation(listener, state as Mutable),
    run: (revision, token) => setListenerAdministrativeState([listener.name], state, revision, token, persist),
    succeeded: () => runtimeState(`Listener ${listener.name} is now ${ADMIN_BADGES[state].label.toLowerCase()}.`, persist),
  }
}

export function poolState(pool: MonitoringPool, state: AdministrativeState, persistOverlay = false): ActionPlan<MutationResponse> {
  const persist = persistOverlay && state !== 'drain'
  return {
    scope: 'admission',
    failure: `set pool ${pool.name} to ${ADMIN_BADGES[state].label.toLowerCase()}`,
    confirm: state === 'ready' ? undefined : confirmations.poolStateConfirmation(pool, state as Mutable),
    run: (revision, token) => setPoolAdministrativeState([pool.name], state, revision, token, persist),
    succeeded: () => runtimeState(`Pool ${pool.name} is now ${ADMIN_BADGES[state].label.toLowerCase()}.`, persist),
  }
}

export function evacuatePool(pool: MonitoringPool, force: boolean): ActionPlan<EvacuationResponse> {
  return {
    scope: 'admission',
    failure: `evacuate pool ${pool.name}`,
    confirm: confirmations.poolEvacuationConfirmation(pool, force),
    run: (revision, token) => evacuatePools([pool.name], force, revision, token),
    succeeded: (report) => evacuationSuccess(`pool ${pool.name}`, report),
  }
}

export function serverState(entry: ServerInventoryEntry, state: AdministrativeState, persistOverlay = false): ActionPlan<MutationResponse> {
  const persist = persistOverlay && state !== 'drain'
  return {
    scope: 'servers',
    failure: `set ${serverLabel(entry)} to ${ADMIN_BADGES[state].label.toLowerCase()}`,
    confirm: state === 'ready' ? undefined : confirmations.serverStateConfirmation(entry, state as Mutable),
    run: (revision, token) => setServerAdministrativeState(target(entry), state, revision, token, persist),
    succeeded: () => runtimeState(`Server ${entry.server.name} in pool ${entry.pool} is now ${ADMIN_BADGES[state].label.toLowerCase()}.`, persist),
  }
}

export function serverHealth(entry: ServerInventoryEntry, health: HealthOverride, persistOverlay = false): ActionPlan<MutationResponse> {
  return {
    scope: 'servers',
    failure: `set the health of ${serverLabel(entry)} to ${health === 'auto' ? 'automatic' : `forced ${health}`}`,
    confirm: health === 'down' ? confirmations.forceDownConfirmation(entry) : undefined,
    run: (revision, token) => setServerHealthOverride(target(entry), health, revision, token, persistOverlay),
    succeeded: () => runtimeState(
      `Health of server ${entry.server.name} in pool ${entry.pool} is ${health === 'auto' ? 'automatic' : `forced ${health}`}.`,
      persistOverlay,
    ),
  }
}

export function serverChecks(entry: ServerInventoryEntry, persistOverlay = false): ActionPlan<MutationResponse> {
  const enabled = !entry.server.checksEnabled
  const verb = enabled ? 'enable' : 'disable'
  return {
    scope: 'servers',
    failure: `${verb} the checks of ${serverLabel(entry)}`,
    run: (revision, token) => setServerChecks(target(entry), enabled, revision, token, persistOverlay),
    succeeded: () => runtimeState(`Checks of server ${entry.server.name} in pool ${entry.pool} are ${enabled ? 'enabled' : 'disabled'}.`, persistOverlay),
  }
}

export function serverDns(entry: ServerInventoryEntry): ActionPlan<DnsRefreshResponse> {
  return {
    scope: 'servers',
    failure: `refresh the DNS of ${serverLabel(entry)}`,
    run: (revision, token) => refreshServerDns(target(entry), revision, token),
    succeeded: (response) => dnsSuccess(entry, response),
  }
}

export function serverLimit(entry: ServerInventoryEntry, limit: number | null, persistOverlay = false): ActionPlan<MutationResponse> {
  return {
    scope: 'servers',
    failure: `${limit === null ? 'reset' : 'set'} the connection limit of ${serverLabel(entry)}`,
    run: (revision, token) => setServerMaxConnections(target(entry), limit, revision, token, persistOverlay),
    succeeded: () => runtimeState(limit === null
      ? `Connection limit of server ${entry.server.name} in pool ${entry.pool} is back to its configured value.`
      : `Connection limit of server ${entry.server.name} in pool ${entry.pool} is ${plural(limit, 'connection')}.`, persistOverlay),
  }
}

export function serverWeight(entry: ServerInventoryEntry, weight: number | null, persistOverlay = false): ActionPlan<MutationResponse> {
  return {
    scope: 'servers',
    failure: `${weight === null ? 'reset' : 'set'} the weight of ${serverLabel(entry)}`,
    run: (revision, token) => setServerWeight(target(entry), weight, revision, token, persistOverlay),
    succeeded: () => runtimeState(weight === null
      ? `Weight of server ${entry.server.name} in pool ${entry.pool} is back to its configured value.`
      : `Weight of server ${entry.server.name} in pool ${entry.pool} is ${weight}.`, persistOverlay),
  }
}

export function evacuateServer(entry: ServerInventoryEntry, force: boolean): ActionPlan<EvacuationResponse> {
  return {
    scope: 'servers',
    failure: `evacuate ${serverLabel(entry)}`,
    confirm: confirmations.serverEvacuationConfirmation(entry, force),
    run: (revision, token) => evacuateServers(target(entry), force, revision, token),
    succeeded: (report) => evacuationSuccess(`server ${entry.server.name} in pool ${entry.pool}`, report),
  }
}
