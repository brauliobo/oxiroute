import type { MeasuredModel, MeasuredRow } from './derive'
import type { ServerRow } from './model'
import type { ViewState } from './urlState'

// What the viewer sees: the rows of the measured model narrowed by the search text, the status chips and "problems
// only", then ordered by the chosen column. Pure functions; the input is never modified.
//
// - Text: split at whitespace, every word must occur (case-insensitive, literally) in the row's text. A server's text
//   is its name, pool, address, status word and origin; a listener's its name, protocol, service, bind and state.
// - A pool stays when at least one of its servers passes; its `servers` are the passing ones (its own totals are
//   unchanged). A pool without servers passes the text on its name, and fails any status chip or "problems only".
// - Status chips narrow servers only; listeners ignore them.
// - Columns: see SORT_COLUMNS. Rows whose value is unknown go last, whichever the direction. Status sorts by severity,
//   worst first when ascending; names sort naturally (case-insensitive, numbers by value).

export const SORT_COLUMNS = ['name', 'status', 'active', 'rate', 'errors', 'p99', 'received', 'sent'] as const
export type SortColumn = (typeof SORT_COLUMNS)[number]

type Pool = MeasuredModel['pools'][number]
type Listener = MeasuredModel['listeners'][number]
type Server = MeasuredRow<ServerRow>
type Sortable = Pool | Listener | Server

const SEVERITY = ['down', 'ejected', 'unknown', 'maint', 'drain', 'unchecked', 'up']
const LISTENER_SEVERITY = ['failed', 'stopped', 'configured', 'listening']
const collator = new Intl.Collator('en', { numeric: true, sensitivity: 'base' })

export function queryPools(pools: readonly Pool[], view: ViewState): Pool[] {
  const words = searchWords(view.search)
  const serverFilter = view.states.length > 0 || view.problemsOnly
  const passes = (server: Server) =>
    matches(words, [server.name, server.pool, server.wire.address, server.statusLabel, server.wire.origin]) &&
    (view.states.length === 0 || view.states.includes(server.wire.status)) &&
    (!view.problemsOnly || server.problem)
  const kept = pools.flatMap((pool) => {
    const servers = pool.servers.filter(passes)
    const emptyPoolPasses = pool.servers.length === 0 && !serverFilter && matches(words, [pool.name])
    return servers.length > 0 || emptyPoolPasses ? [{ ...pool, servers }] : []
  })
  return sorted(kept, view).map((pool) => ({ ...pool, servers: sorted(pool.servers, view) }))
}

export function queryListeners(listeners: readonly Listener[], view: ViewState): Listener[] {
  const words = searchWords(view.search)
  const kept = listeners.filter((listener) =>
    matches(words, [listener.name, listener.wire.protocol, listener.wire.service, listener.wire.bind, listener.wire.state]) &&
    (!view.problemsOnly || listener.problem))
  return sorted(kept, view)
}

const searchWords = (search: string) => search.toLowerCase().split(/\s+/).filter(Boolean)

const matches = (words: string[], texts: Array<string | undefined>) => {
  const text = texts.filter(Boolean).join('\n').toLowerCase()
  return words.every((word) => text.includes(word))
}

function sorted<Row extends Sortable>(rows: Row[], { sort }: ViewState): Row[] {
  const column = SORT_COLUMNS.find((name) => name === sort?.column)
  if (!column) return rows
  const sign = sort!.direction === 'desc' ? -1 : 1
  return rows.map((row, index) => ({ row, index, value: valueOf(row, column) })).sort((left, right) => {
    if (left.value === undefined || right.value === undefined) {
      return Number(left.value === undefined) - Number(right.value === undefined) || left.index - right.index
    }
    const order = typeof left.value === 'string' ? collator.compare(left.value, right.value as string) : left.value - (right.value as number)
    return sign * order || left.index - right.index
  }).map(({ row }) => row)
}

function valueOf(row: Sortable, column: SortColumn): string | number | undefined {
  switch (column) {
    case 'name': return row.name
    case 'status': return severity(row)
    case 'active': return row.active
    case 'rate': return row.rates.requests
    case 'errors': return row.rates.errors
    case 'p99': return row.latency.request?.p99.ms
    case 'received': return row.rates.bytesReceived
    case 'sent': return row.rates.bytesSent
  }
}

function severity(row: Sortable): number {
  const wire = row.wire as { status?: string; state?: string }
  return wire.status !== undefined ? SEVERITY.indexOf(wire.status) : LISTENER_SEVERITY.indexOf(wire.state ?? '')
}
