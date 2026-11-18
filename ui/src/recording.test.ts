import { describe, expect, it } from 'vitest'

import { recorderPhaseTone, relayPhaseTone } from './recording'

describe('recording tones', () => {
  it('maps every recorder phase to a tone, healthy recording ok and failure bad', () => {
    expect(recorderPhaseTone('recording')).toBe('ok')
    expect(recorderPhaseTone('failed')).toBe('bad')
    expect(recorderPhaseTone('starting')).toBe('warn')
    expect(recorderPhaseTone('stopping')).toBe('warn')
    expect(recorderPhaseTone('idle')).toBe('neutral')
  })

  it('maps every relay phase to a tone', () => {
    expect(relayPhaseTone('connecting')).toBe('warn')
    expect(relayPhaseTone('publishing')).toBe('ok')
    expect(relayPhaseTone('pulling')).toBe('ok')
    expect(relayPhaseTone('backoff')).toBe('warn')
    expect(relayPhaseTone('stopped')).toBe('neutral')
  })
})
