import { UNKNOWN, formatAge, formatDuration, formatTimestamp } from '#design/format'
import { pluralize } from '../../formatters'
import type { Fact } from '../../workspace/types'
import type { Camera } from './model'

const picture = ({ resolution, frameRate }: Camera) =>
  [resolution, frameRate].filter(Boolean).join(' at ') || UNKNOWN

const codecs = ({ video, audio }: Camera) => `${video} / ${audio}`

function liveFacts(camera: Camera): Fact[] {
  return [
    { label: 'Picture', value: picture(camera) },
    { label: 'Codecs', value: codecs(camera) },
    { label: 'Bitrate', value: camera.bitrate ?? UNKNOWN },
    { label: 'Publisher', value: camera.publisher?.peer ?? (camera.publisher ? 'Unknown address' : 'No publisher') },
    { label: 'Viewers', value: pluralize(camera.viewers, 'viewer') },
    { label: 'Uptime', value: camera.uptimeMs === null ? UNKNOWN : formatDuration(camera.uptimeMs) },
  ]
}

function offlineFacts(camera: Camera, now: number): Fact[] {
  return [
    { label: 'Last seen', value: camera.lastSeenAt === null ? UNKNOWN : formatAge(Math.max(0, now - camera.lastSeenAt)) },
    { label: 'Ended', value: camera.endedAt === null ? UNKNOWN : formatTimestamp(camera.endedAt) },
    { label: 'Why', value: camera.endReason ?? UNKNOWN },
    { label: 'Last picture', value: picture(camera) },
    { label: 'Codecs', value: codecs(camera) },
    { label: 'Bitrate', value: camera.bitrate ?? UNKNOWN },
  ]
}

export const cameraFacts = (camera: Camera, now: number): Fact[] =>
  camera.status === 'offline' ? offlineFacts(camera, now) : liveFacts(camera)
