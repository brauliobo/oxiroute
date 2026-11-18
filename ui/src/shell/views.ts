export const VIEWS = [
  { id: 'overview', label: 'Overview', title: 'Runtime observatory' },
  { id: 'cameras', label: 'Cameras', title: 'Cameras and recordings' },
  { id: 'stats', label: 'Statistics', title: 'Statistics' },
  { id: 'operations', label: 'Operations', title: 'Operations' },
  { id: 'certificates', label: 'Certificates', title: 'Certificate inventory' },
  { id: 'events', label: 'Events', title: 'Operational event history' },
  { id: 'audit', label: 'Audit', title: 'Durable audit history' },
  { id: 'configuration', label: 'Configuration', title: 'Canonical configuration' },
  { id: 'provenance', label: 'Provenance', title: 'Source provenance' },
] as const

export type ViewId = (typeof VIEWS)[number]['id']

export const NAV_ITEMS = VIEWS.map(({ id, label }) => ({ id, label, href: `#/${id}` }))

export const viewFromHash = (hash: string): ViewId => VIEWS.find(({ id }) => `#/${id}` === hash)?.id ?? 'overview'

export const viewTitle = (view: ViewId): string => VIEWS.find(({ id }) => id === view)!.title

export const documentTitle = (view: ViewId): string => `${viewTitle(view)} - OxiRoute`
