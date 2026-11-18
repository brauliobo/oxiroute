import type {
  GenerationStatus,
  ListenerInventoryResponse,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryResponse,
} from '../api'
import type { ConfirmRequest } from '../shell/confirm'

export type Scope = 'generation' | 'admission' | 'servers'
export type OutcomeTone = 'ok' | 'warn' | 'bad'

export interface ActionOutcome {
  scope: Scope
  tone: OutcomeTone
  summary: string
  details: readonly string[]
}

export interface Success {
  tone: 'ok' | 'warn'
  summary: string
  details?: readonly string[]
}

export interface ActionPlan<Result> {
  scope: Scope
  failure: string
  confirm?: ConfirmRequest
  run: (revision: string, token: string) => Promise<Result>
  succeeded: (result: Result) => Success
}

export interface Snapshot {
  status: RuntimeStatus | null
  generation: GenerationStatus | null
  listeners: ListenerInventoryResponse | null
  pools: PoolInventoryResponse | null
  servers: ServerInventoryResponse | null
}
