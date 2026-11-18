import { afterEach, describe, expect, it, vi } from 'vitest'

import { saveConfig } from './api'
import { canonicalConfig, restartReason } from './test/configurationWorkspace'
import { jsonResponse } from './test/contractFixtures'

const saved = {
  diskRevision: 'disk',
  candidateRevision: 'candidate',
  activeRevision: 'active',
  diagnostics: [],
}
const pending = {
  ...saved,
  outcome: 'saved_pending_activation',
  activationState: 'pending',
  restartRequired: false,
  restartReasons: [],
}
const restart = {
  ...saved,
  outcome: 'saved_restart_required',
  activationState: 'restart_required',
  restartRequired: true,
  restartReasons: [restartReason],
}

async function save(payload: unknown) {
  vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(payload)))
  return saveConfig(canonicalConfig(), 'disk', 'token')
}

describe('configuration save response decoding', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('keeps the restart reasons of a restart-required save', async () => {
    expect(await save(restart)).toEqual(restart)
    expect(await save(pending)).toEqual(pending)
  })

  it('rejects a save without restart reasons', async () => {
    const { restartReasons: _omitted, ...withoutReasons } = pending
    await expect(save(withoutReasons)).rejects.toThrow('invalid response payload')
  })

  it('rejects restart reasons that disagree with the outcome', async () => {
    await expect(save({ ...restart, restartReasons: [] })).rejects.toThrow('invalid response payload')
    await expect(save({ ...pending, restartReasons: [restartReason] })).rejects.toThrow('invalid response payload')
    await expect(save({ ...restart, restartReasons: [{ code: 'x' }] })).rejects.toThrow('invalid response payload')
  })
})
