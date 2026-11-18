import type {
  GenerationStatus,
  ListenerInventoryResponse,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryResponse,
} from '../api'
import type { ActionOutcome as Outcome, ActionPlan as Plan } from '../workspace/actions'

export type { Success } from '../workspace/actions'
export type Scope = 'generation' | 'admission' | 'servers'
export type ActionOutcome = Outcome<Scope>
export type ActionPlan<Result> = Plan<Scope, string, Result>

export interface Snapshot {
  status: RuntimeStatus | null
  generation: GenerationStatus | null
  listeners: ListenerInventoryResponse | null
  pools: PoolInventoryResponse | null
  servers: ServerInventoryResponse | null
}
