import type { ListenerRuntimeState, TopologyRuntimeStatus } from './api'

export const listenerStateLabels: Record<ListenerRuntimeState, string> = {
  configured: 'Configured',
  listening:  'Listening',
  stopped:    'Stopped',
  failed:     'Failed',
}

export const topologyRuntimeStatusLabels: Record<TopologyRuntimeStatus, string> = {
  active:   'Active',
  starting: 'Starting',
  degraded: 'Degraded',
}
