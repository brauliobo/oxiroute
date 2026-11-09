import { afterEach, describe, expect, it, vi } from 'vitest'
import '../../test/dialog'
import { flushPromises, mount } from '../../test/mount'
import { decodeSnapshot } from '../decode'
import { wireDocument } from '../fixtures'
import { buildModel } from '../model'
import AdminActions from './AdminActions.vue'

const pools = buildModel(decodeSnapshot(wireDocument({ admin: true }))).pools
const props = { pools, revision: 'rev-1' }
const respond = (status = 204, body?: unknown) =>
  vi.fn<typeof fetch>(async () => new Response(body === undefined ? null : JSON.stringify(body), { status }))
const button = (wrapper: ReturnType<typeof mount>, text: string) =>
  wrapper.findAll('button').find((candidate) => candidate.text() === text)!
afterEach(() => { vi.unstubAllGlobals() })

describe('AdminActions', () => {
  it('is a labelled section with a pool and a server choice and the three states', () => {
    const wrapper = mount(AdminActions, { props })
    expect(wrapper.get('section').attributes('aria-labelledby')).toBe(wrapper.get('h2').attributes('id'))
    expect(wrapper.get('h2').text()).toBe('Administration')
    expect(wrapper.findAll('label').map((label) => label.text())).toEqual(['Pool', 'Server'])
    expect(wrapper.findAll('select')[0]!.findAll('option').map((option) => option.text())).toEqual(['web', 'api'])
    expect(wrapper.findAll('select')[1]!.findAll('option').map((option) => option.text())).toEqual(['origin-a', 'origin-b'])
    expect(['Ready', 'Drain', 'Maintenance'].map((text) => button(wrapper, text).exists())).toEqual([true, true, true])
    expect(wrapper.text()).toContain('Current state: ready')
  })

  it('follows the chosen pool and server', async () => {
    const wrapper = mount(AdminActions, { props })
    await wrapper.findAll('select')[0]!.setValue('api')
    expect(wrapper.findAll('select')[1]!.findAll('option').map((option) => option.text())).toEqual(['origin-c'])
    await wrapper.findAll('select')[0]!.setValue('web')
    await wrapper.findAll('select')[1]!.setValue('origin-b')
    expect(wrapper.text()).toContain('Current state: drain')
  })

  it('asks for confirmation naming the server and the state, and posts nothing on cancel', async () => {
    const fetcher = respond()
    vi.stubGlobal('fetch', fetcher)
    const wrapper = mount(AdminActions, { props })
    await button(wrapper, 'Drain').trigger('click')
    const dialog = wrapper.get('dialog')
    expect(dialog.attributes('open')).toBeDefined()
    expect(dialog.text()).toContain('Set server origin-a of pool web to Drain?')
    await dialog.findAll('button').find((candidate) => candidate.text() === 'Cancel')!.trigger('click')
    expect(dialog.attributes('open')).toBeUndefined()
    expect(fetcher).not.toHaveBeenCalled()
    expect(wrapper.emitted('changed')).toBeUndefined()
  })

  it('posts the revision, pool, server and state on confirm, reports it and asks for a refresh', async () => {
    const fetcher = respond()
    vi.stubGlobal('fetch', fetcher)
    const wrapper = mount(AdminActions, { props })
    await button(wrapper, 'Maintenance').trigger('click')
    await wrapper.get('dialog').findAll('button').find((candidate) => candidate.text() === 'Confirm')!.trigger('click')
    await flushPromises()
    const [url, init] = fetcher.mock.calls[0]!
    expect(url).toBe('/')
    expect(Object.fromEntries(init!.body as URLSearchParams)).toEqual({
      generation_revision: 'rev-1', pool: 'web', server: 'origin-a', state: 'maintenance',
    })
    expect(wrapper.get('[role=status]').text()).toBe('Server origin-a of pool web is now in maintenance.')
    expect(wrapper.emitted('changed')).toHaveLength(1)
    expect(wrapper.get('dialog').attributes('open')).toBeUndefined()
  })

  it('shows the refusal as an alert, and a changed revision also refreshes the page', async () => {
    vi.stubGlobal('fetch', respond(409, { error: { code: 'generation_revision_conflict', message: 'x' } }))
    const wrapper = mount(AdminActions, { props })
    await button(wrapper, 'Ready').trigger('click')
    await wrapper.get('dialog').findAll('button').find((candidate) => candidate.text() === 'Confirm')!.trigger('click')
    await flushPromises()
    expect(wrapper.get('[role=alert]').text()).toContain('changed since this page loaded')
    expect(wrapper.emitted('changed')).toHaveLength(1)
  })

  it('does not ask for a refresh when the request never reached the server state', async () => {
    vi.stubGlobal('fetch', respond(403, { error: { code: 'admin_forbidden', message: 'x' } }))
    const wrapper = mount(AdminActions, { props })
    await button(wrapper, 'Ready').trigger('click')
    await wrapper.get('dialog').findAll('button').find((candidate) => candidate.text() === 'Confirm')!.trigger('click')
    await flushPromises()
    expect(wrapper.get('[role=alert]').text()).toContain('only from this machine')
    expect(wrapper.emitted('changed')).toBeUndefined()
  })

  it('shows names as text', () => {
    const hostile = buildModel(decodeSnapshot(wireDocument({ admin: true }))).pools.slice(0, 1)
    hostile[0] = { ...hostile[0]!, name: '<img src=x onerror=alert(1)>' }
    const wrapper = mount(AdminActions, { props: { pools: hostile, revision: 'r' } })
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toContain('<img src=x onerror=alert(1)>')
  })
})
