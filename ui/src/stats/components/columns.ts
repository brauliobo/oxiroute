import type { SortColumn } from '../query'

// Column headers of the tables. `column` is the store's sort column; headers without one do not sort.

export interface Column { label: string; column?: SortColumn }

export const SERVER_COLUMNS: readonly Column[] = [
  { label: 'Server', column: 'name' }, { label: 'Status', column: 'status' }, { label: 'Sessions', column: 'active' },
  { label: 'Req/s', column: 'rate' }, { label: 'In', column: 'received' }, { label: 'Out', column: 'sent' },
  { label: 'Responses' }, { label: 'Errors', column: 'errors' }, { label: 'Check' }, { label: 'Weight' },
  { label: 'Downtime' }, { label: 'Latency', column: 'p99' },
]

export const LISTENER_COLUMNS: readonly Column[] = [
  { label: 'Listener', column: 'name' }, { label: 'State', column: 'status' }, { label: 'Connections', column: 'active' },
  { label: 'Req/s', column: 'rate' }, { label: 'In', column: 'received' }, { label: 'Out', column: 'sent' },
  { label: 'Errors', column: 'errors' }, { label: 'Latency', column: 'p99' },
]
