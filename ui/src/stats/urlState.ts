import type { ServerStatus } from './generated/statsPage'

// Everything the viewer can change lives in the query string, so a link restores the view. Nothing in it is secret.
// The defaults are never written. Input from the URL is untrusted: invalid values are dropped, not repaired.
//
//   q=<text>  state=up,down  problems=1  sort=<column> | sort=-<column>  paused=1  interval=<ms>
//   theme=light|dark  open=<row key> (repeatable)  closed=<row key> (repeatable)

export const MAX_SEARCH_LENGTH = 200
export const MAX_KEYS = 256
export const MIN_INTERVAL_MS = 1000
export const MAX_INTERVAL_MS = 86_400_000

export const SERVER_STATUSES = ['up', 'down', 'drain', 'maint', 'ejected', 'unchecked', 'unknown'] as const satisfies
  readonly ServerStatus[]
export type StatusFilter = (typeof SERVER_STATUSES)[number]
export const THEMES = ['auto', 'light', 'dark'] as const
export type Theme = (typeof THEMES)[number]
export interface SortSpec {
  column: string
  direction: 'asc' | 'desc'
}

export interface ViewState {
  search: string
  states: StatusFilter[]
  problemsOnly: boolean
  sort: SortSpec | undefined
  paused: boolean
  /** The viewer's polling interval; undefined follows the server's `intervalHintMs`. */
  intervalMs: number | undefined
  theme: Theme
  /** Row keys (see model.ts) whose details are open. */
  expanded: string[]
  /** Group (pool) keys that are folded. */
  collapsed: string[]
}

export const defaultView = (): ViewState => ({
  search: '',
  states: [],
  problemsOnly: false,
  sort: undefined,
  paused: false,
  intervalMs: undefined,
  theme: 'auto',
  expanded: [],
  collapsed: [],
})

const COLUMN = /^[A-Za-z][A-Za-z0-9_]{0,31}$/

export function parseViewState(search: string): ViewState {
  const params = new URLSearchParams(search)
  const view = defaultView()
  view.search = limitCodePoints(params.get('q') ?? '', MAX_SEARCH_LENGTH)
  const states = new Set((params.get('state') ?? '').split(','))
  view.states = SERVER_STATUSES.filter((status) => states.has(status))
  view.problemsOnly = params.get('problems') === '1'
  view.sort = parseSort(params.get('sort'))
  view.paused = params.get('paused') === '1'
  view.intervalMs = parseInterval(params.get('interval'))
  view.theme = THEMES.find((theme) => theme === params.get('theme') && theme !== 'auto') ?? 'auto'
  view.expanded = keys(params.getAll('open'))
  view.collapsed = keys(params.getAll('closed'))
  return view
}

/** `''` for the default view, otherwise `?...` with a fixed key order. */
export function formatViewState(view: ViewState): string {
  const params = new URLSearchParams()
  if (view.search) params.set('q', view.search)
  if (view.states.length > 0) params.set('state', SERVER_STATUSES.filter((status) => view.states.includes(status)).join(','))
  if (view.problemsOnly) params.set('problems', '1')
  if (view.sort) params.set('sort', `${view.sort.direction === 'desc' ? '-' : ''}${view.sort.column}`)
  if (view.paused) params.set('paused', '1')
  if (view.intervalMs !== undefined) params.set('interval', String(view.intervalMs))
  if (view.theme !== 'auto') params.set('theme', view.theme)
  for (const key of view.expanded) params.append('open', key)
  for (const key of view.collapsed) params.append('closed', key)
  const query = params.toString()
  return query ? `?${query}` : ''
}

interface UrlTarget {
  location: Pick<Location, 'pathname' | 'search' | 'hash'>
  history: Pick<History, 'state' | 'replaceState'>
}

/** Writes the view into the address bar without adding a history entry; does nothing when it is already there. */
export function replaceUrlState(view: ViewState, { location, history }: UrlTarget): void {
  const search = formatViewState(view)
  if (search !== location.search) history.replaceState(history.state, '', `${location.pathname}${search}${location.hash}`)
}

function parseSort(value: string | null): SortSpec | undefined {
  const descending = value?.startsWith('-') ?? false
  const column = descending ? value!.slice(1) : value
  return column && COLUMN.test(column) ? { column, direction: descending ? 'desc' : 'asc' } : undefined
}

function parseInterval(value: string | null): number | undefined {
  const interval = value !== null && /^[0-9]+$/.test(value) ? Number(value) : Number.NaN
  return interval >= MIN_INTERVAL_MS && interval <= MAX_INTERVAL_MS ? interval : undefined
}

const keys = (values: string[]) => [...new Set(values.filter(Boolean))].slice(0, MAX_KEYS)

const limitCodePoints = (text: string, max: number) => Array.from(text).slice(0, max).join('')
