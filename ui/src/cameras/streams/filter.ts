import type { StreamStatus } from '#design/status/status'
import type { Camera } from './model'

export type StatusFilter = 'all' | StreamStatus

export interface CameraFilter {
  query: string
  status: StatusFilter
}

export interface Summary {
  all: number
  streaming: number
  degraded: number
  idle: number
  offline: number
  recording: number
}

export const NO_FILTER: CameraFilter = { query: '', status: 'all' }

export function filterCameras(cameras: readonly Camera[], { query, status }: CameraFilter): Camera[] {
  const needle = query.trim().toLowerCase()
  return cameras.filter((camera) =>
    (status === 'all' || camera.status === status) && (needle === '' || camera.title.toLowerCase().includes(needle)))
}

export function summarize(cameras: readonly Camera[]): Summary {
  const count = (status: StreamStatus) => cameras.filter((camera) => camera.status === status).length
  return {
    all: cameras.length,
    streaming: count('streaming'),
    degraded: count('degraded'),
    idle: count('idle'),
    offline: count('offline'),
    recording: cameras.filter(({ recorders }) => recorders.some(({ recording }) => recording)).length,
  }
}
