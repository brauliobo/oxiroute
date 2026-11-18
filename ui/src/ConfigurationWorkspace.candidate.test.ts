import { flushPromises } from './test/mount'
import { describe, expect, it } from 'vitest'

import type { ConfigSnapshot } from './config'
import { configSnapshot, findButton, installConfigFetch, mountUnlocked } from './test/configurationWorkspace'

const hash = (character: string): string => character.repeat(64)

function freshStart(overrides: Partial<ConfigSnapshot> = {}): ConfigSnapshot {
  return {
    ...configSnapshot(),
    configFormat: 'lua',
    diskRevision: hash('a'),
    candidateRevision: hash('b'),
    activeRevision: hash('b'),
    ...overrides,
  }
}

async function bannersFor(snapshot: ConfigSnapshot) {
  installConfigFetch(undefined, 200, snapshot)
  return (await mountUnlocked()).findAll('.revision-banner')
}

describe('candidate-waiting banner', () => {
  it('stays hidden on a fresh start where the file is not canonical so the disk and active hashes differ', async () => {
    expect(await bannersFor(freshStart())).toHaveLength(0)
  })

  it('shows when the file on disk renders to a different generation than the one serving traffic', async () => {
    const banners = await bannersFor(freshStart({ candidateRevision: hash('c') }))

    expect(banners).toHaveLength(1)
    expect(banners[0]!.classes()).toContain('candidate-waiting')
    expect(banners[0]!.text()).toContain('A saved configuration is waiting to be activated.')
    expect(banners[0]!.text()).toContain('cccc')
  })

  it('stays hidden when no generation is serving yet', async () => {
    expect(await bannersFor(freshStart({ activeRevision: null, candidateRevision: hash('c') }))).toHaveLength(0)
  })

  it('is replaced by the save message right after a save, not repeated beside it', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.findAll('.revision-banner')).toHaveLength(1)
    expect(wrapper.get('.revision-banner').classes()).toContain('save-state')
    expect(wrapper.find('.candidate-waiting').exists()).toBe(false)
  })
})
