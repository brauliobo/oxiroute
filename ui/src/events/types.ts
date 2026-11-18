export type StreamState = 'offline' | 'connecting' | 'live' | 'reconnecting'

export interface RingRestart {
  previousLatestCursor: number
  latestCursor: number
}
