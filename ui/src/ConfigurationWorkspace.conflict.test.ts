import { flushPromises } from './test/mount'
import { describe, expect, it, vi } from 'vitest'

import type { CanonicalConfig, ConfigSnapshot } from './config'
import { pendingConfirm, settleConfirm } from './shell/confirm'
import { jsonResponse } from './test/contractFixtures'
import { configSnapshot, findButton, mountUnlocked } from './test/configurationWorkspace'

function changedOnDisk(edit: (config: CanonicalConfig) => void): ConfigSnapshot {
  const snapshot = configSnapshot()
  snapshot.diskRevision = 'disk-222222222222222222222222222222222222222222222222222222222222'
  edit(snapshot.config)
  return snapshot
}

async function staleWorkspace(theirs: ConfigSnapshot) {
  let reads = 0
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
    if (String(input) !== '/api/v1/config') throw new Error(`Unexpected request: ${String(input)}`)
    reads += 1
    return jsonResponse(reads === 1 ? configSnapshot() : theirs)
  }))
  const wrapper = await mountUnlocked(true)
  await wrapper.get('[data-field="version"] input').setValue('3')
  await findButton(wrapper, 'Check disk revision').trigger('click')
  await flushPromises()
  return wrapper
}

const versionOf = (wrapper: Awaited<ReturnType<typeof staleWorkspace>>) =>
  (wrapper.get('[data-field="version"] input').element as HTMLInputElement).value

describe('stale disk revision', () => {
  it('explains the conflict in one banner and offers reapply and discard', async () => {
    const wrapper = await staleWorkspace(changedOnDisk((config) => { config.management = { bind: '127.0.0.1:9181', ui_dir: null } }))

    expect(wrapper.findAll('.revision-banner.stale, .revision-banner.error, .revision-banner.save-state')).toHaveLength(1)
    const banner = wrapper.get('.revision-banner.stale')
    expect(banner.text()).toContain('The file on disk changed while you were editing')
    expect(banner.findAll('button').map((button) => button.text()))
      .toEqual(['Reload and reapply my edits', 'Discard my edits and load disk'])
  })

  it('reloads the new disk file and reapplies edits that do not collide, listing them', async () => {
    const wrapper = await staleWorkspace(changedOnDisk((config) => { config.management = { bind: '127.0.0.1:9181', ui_dir: null } }))

    await findButton(wrapper, 'Reload and reapply my edits').trigger('click')
    await flushPromises()

    expect(versionOf(wrapper)).toBe('3')
    expect((wrapper.vm as unknown as { draft: CanonicalConfig }).draft.management?.bind).toBe('127.0.0.1:9181')
    expect(wrapper.find('.revision-banner.stale').exists()).toBe(false)
    const report = wrapper.get('.reapply-report')
    expect(report.text()).toContain('Reapplied 1 change; nothing collided.')
    expect(report.text()).toContain('/version')
    expect(wrapper.get('.revision-board').text()).toContain('disk-222')
  })

  it('keeps the disk value where both changed the same setting and lists the collision', async () => {
    const wrapper = await staleWorkspace(changedOnDisk((config) => { config.version = 2 }))

    await findButton(wrapper, 'Reload and reapply my edits').trigger('click')
    await flushPromises()

    expect(versionOf(wrapper)).toBe('2')
    const report = wrapper.get('.reapply-report')
    expect(report.text()).toContain('Reapplied 0 changes; 1 change collided')
    expect(report.text()).toContain('/version')
    expect(report.text()).toContain('disk 2, yours 3')
    await findButton(wrapper, 'Dismiss').trigger('click')
    expect(wrapper.find('.reapply-report').exists()).toBe(false)
  })

  it('discards the draft only after naming what is lost', async () => {
    const wrapper = await staleWorkspace(changedOnDisk((config) => { config.version = 2 }))

    await findButton(wrapper, 'Discard my edits and load disk').trigger('click')
    expect(pendingConfirm.value?.title).toBe('Discard your draft and load the disk file?')
    expect(pendingConfirm.value?.details).toEqual(['/version'])
    settleConfirm(false)
    await flushPromises()
    expect(versionOf(wrapper)).toBe('3')
    expect(wrapper.find('.revision-banner.stale').exists()).toBe(true)

    await findButton(wrapper, 'Discard my edits and load disk').trigger('click')
    settleConfirm(true)
    await flushPromises()
    expect(versionOf(wrapper)).toBe('2')
    expect(wrapper.find('.revision-banner.stale').exists()).toBe(false)
    expect(wrapper.find('.reapply-report').exists()).toBe(false)
  })
})
