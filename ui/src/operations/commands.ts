import type {
  AdministrativeState,
  HealthOverride,
  MonitoringListener,
  MonitoringPool,
  ServerInventoryEntry,
} from '../api'
import * as plans from './plans'
import type { ActionPlan, Snapshot } from './types'

export type GenerationCommand =
  | { kind: 'reload' }
  | { kind: 'rollback' }
  | { kind: 'drain-generation' }
  | { kind: 'drain-process' }
  | { kind: 'shutdown' }

export type AdmissionCommand =
  | { kind: 'listener-state'; listener: MonitoringListener; state: AdministrativeState }
  | { kind: 'pool-state'; pool: MonitoringPool; state: AdministrativeState }
  | { kind: 'pool-evacuate'; pool: MonitoringPool }

export type ServerCommand =
  | { kind: 'state'; entry: ServerInventoryEntry; state: AdministrativeState }
  | { kind: 'health'; entry: ServerInventoryEntry; health: HealthOverride }
  | { kind: 'checks'; entry: ServerInventoryEntry }
  | { kind: 'dns'; entry: ServerInventoryEntry }
  | { kind: 'limit'; entry: ServerInventoryEntry; limit: number | null }
  | { kind: 'weight'; entry: ServerInventoryEntry; weight: number | null }
  | { kind: 'evacuate'; entry: ServerInventoryEntry }

export type Command = GenerationCommand | AdmissionCommand | ServerCommand

export interface CommandContext {
  snapshot: Snapshot
  force: boolean
  perform: <Result>(plan: ActionPlan<Result>) => Promise<void>
}

export function dispatch(command: Command, { snapshot, force, perform }: CommandContext): Promise<void> {
  switch (command.kind) {
    case 'reload': return perform(plans.reload(snapshot))
    case 'rollback': return perform(plans.rollback(snapshot))
    case 'drain-generation': return perform(plans.drainActiveGeneration(snapshot))
    case 'drain-process': return perform(plans.drainRunningProcess(snapshot))
    case 'shutdown': return perform(plans.shutdown(snapshot))
    case 'listener-state': return perform(plans.listenerState(command.listener, command.state))
    case 'pool-state': return perform(plans.poolState(command.pool, command.state))
    case 'pool-evacuate': return perform(plans.evacuatePool(command.pool, force))
    case 'state': return perform(plans.serverState(command.entry, command.state))
    case 'health': return perform(plans.serverHealth(command.entry, command.health))
    case 'checks': return perform(plans.serverChecks(command.entry))
    case 'dns': return perform(plans.serverDns(command.entry))
    case 'limit': return perform(plans.serverLimit(command.entry, command.limit))
    case 'weight': return perform(plans.serverWeight(command.entry, command.weight))
    case 'evacuate': return perform(plans.evacuateServer(command.entry, force))
  }
}
