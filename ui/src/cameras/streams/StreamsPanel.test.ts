import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { clearToasts, toasts } from '#design/toast/state'
import '../../test/dialog'
import { camerasCatalog, camerasStats } from '../../test/camerasFixtures'
import { managementGeneration } from '../../test/managementFixtures'
import { flushPromises, mount } from '../../test/mount'
import type { VaporWrapper } from '../../test/mount'
import ConfirmHost from '../../shell/ConfirmHost.vue'
import { pendingConfirm, settleConfirm } from '../../shell/confirm'
import StreamsPanel from './StreamsPanel.vue'

const NOW = 1_750_000_000_000
const TICKET = '1750000600000.c2lnbmF0dXJl'

interface Call { url: string; method: string; headers: Record<string, string>; body: unknown }
type Reply = (call: Call) => Response | undefined

function install(reply: Reply = () => undefined) {
  const calls: Call[] = []
  const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const call: Call = {
      url: String(input),
      method: init?.method ?? 'GET',
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: init?.body ? JSON.parse(String(init.body)) : null,
    }
    calls.push(call)
    return Promise.resolve(reply(call) ?? fallback(call))
  })
  vi.stubGlobal('fetch', fetch)
  return { calls, mutations: () => calls.filter((call) => call.method === 'POST') }
}

function fallback(call: Call): Response {
  if (call.url === '/api/v1/generations') return Response.json(managementGeneration())
  if (call.url.endsWith('/ticket')) {
    const path = call.url.replace('/ticket', '/index.m3u8')
    return Response.json({ ticket: TICKET, path: `${path}?ticket=${TICKET}`, expiresAtUnixMs: NOW + 600_000, ttlSeconds: 600 })
  }
  if (call.url.includes('/recorders/')) return Response.json(recorderReply())
  if (call.url.includes('/drop')) {
    const [, , , , , , session, target] = call.url.split('/')
    return Response.json({ outcome: 'requested', sessionId: session, target: target === 'drop' ? 'client' : target, sessionRevision: '11' }, { status: 202 })
  }
  return Response.json({})
}

const recorderReply = () => camerasCatalog(NOW).streams[0]!.recorders[0]!

function open(props: Record<string, unknown> = {}) {
  const reload = vi.fn(async () => undefined)
  const wrapper = mount(StreamsPanel, {
    props: {
      token: 'test-token',
      catalog: camerasCatalog(NOW),
      stats: camerasStats(NOW),
      catalogError: null,
      statsError: null,
      nowUnixMs: NOW,
      refreshing: false,
      reload,
      ...props,
    },
  })
  return { wrapper, reload }
}

const tile = (wrapper: VaporWrapper, name: string) => wrapper.get(`[data-camera="${name}"]`)
const names = (wrapper: VaporWrapper) => wrapper.findAll('[data-camera]').map((element) => element.attributes('data-camera'))

async function answer(confirmed: boolean) {
  await flushPromises()
  const asked = pendingConfirm.value
  settleConfirm(confirmed)
  await flushPromises()
  return asked
}

beforeEach(() => {
  vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockReturnValue('maybe')
})

afterEach(() => {
  settleConfirm(false)
  clearToasts()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

describe('streams grid', () => {
  it('shows one tile per stream with its state word, problems first', () => {
    install()
    const { wrapper } = open()
    expect(names(wrapper)).toEqual(['cam-garage', 'cam-side', 'cam-front', 'cam-spare', 'cam-yard', 'cam-lobby'])
    expect(tile(wrapper, 'cam-front').get('.ox-badge').text()).toBe('LIVE')
    expect(tile(wrapper, 'cam-garage').get('.ox-badge').text()).toBe('DEGRADED')
    expect(tile(wrapper, 'cam-yard').get('.ox-badge').text()).toBe('OFFLINE')
    expect(tile(wrapper, 'cam-spare').get('.ox-badge').text()).toBe('IDLE')
  })

  it('shows codec, picture, bitrate, publisher, viewers and uptime on a live tile', () => {
    install()
    const { wrapper } = open()
    const text = tile(wrapper, 'cam-front').text()
    for (const part of ['1920x1080 at 30 fps', 'AVC / AAC', '4.1 Mbit/s', '192.0.2.10', '3 viewers', '1h 0m']) {
      expect(text).toContain(part)
    }
  })

  it('shows an offline camera from the catalog with when and why it ended and no controls', () => {
    install()
    const { wrapper } = open()
    const yard = tile(wrapper, 'cam-yard')
    expect(yard.text()).toContain('5m 0s ago')
    expect(yard.text()).toContain('The publisher stopped sending media.')
    expect(yard.text()).toContain('No picture: the publisher is gone.')
    expect(yard.findAll('button')).toHaveLength(0)
  })

  it('explains a degraded stream with its reasons', () => {
    install()
    const { wrapper } = open()
    expect(tile(wrapper, 'cam-side').get('[aria-label="Problems"]').text()).toContain('No media for 40 s')
    expect(tile(wrapper, 'cam-garage').text()).toContain('quota_files')
  })

  it('filters by state chip and by search, and clears both', async () => {
    install()
    const { wrapper } = open()
    expect(wrapper.findAll('[data-filter]').map((chip) => chip.text())).toEqual([
      'All 6', 'Live 1', 'Degraded 2', 'Idle 1', 'Offline 2',
    ])
    await wrapper.get('[data-filter="offline"]').trigger('click')
    expect(names(wrapper)).toEqual(['cam-yard', 'cam-lobby'])
    expect(wrapper.get('[data-filter="offline"]').attributes('aria-pressed')).toBe('true')
    await wrapper.get('[data-filter="all"]').trigger('click')
    await wrapper.get('#cams-find').setValue('garage')
    expect(names(wrapper)).toEqual(['cam-garage'])
    await wrapper.get('#cams-find').setValue('nothing-like-this')
    expect(wrapper.text()).toContain('No stream matches')
    await wrapper.get('.ox-empty button').trigger('click')
    expect(names(wrapper)).toHaveLength(6)
  })

  it('explains an empty catalog, a missing listener and a failing refresh without dropping data', () => {
    install()
    const empty = camerasCatalog(NOW)
    empty.streams = []
    empty.recently_ended_streams = []
    empty.capabilities.live_ingest = false
    const { wrapper } = open({ catalog: empty })
    expect(wrapper.text()).toContain('No streams yet')
    expect(wrapper.text()).toContain('RTMP ingestion is not connected')

    const failing = open({ catalogError: 'The management endpoint is unreachable.', statsError: 'No client list.' })
    expect(failing.wrapper.get('[role="alert"]').text()).toContain('Stream refresh failed.')
    expect(failing.wrapper.get('[role="alert"]').text()).toContain('The last valid streams stay visible.')
    expect(failing.wrapper.get('[role="alert"]').text()).toContain('Client list unavailable.')
    expect(names(failing.wrapper)).toHaveLength(6)
  })

  it('shows a loading state before the first catalog and an unavailable state when none can be had', () => {
    install()
    expect(open({ catalog: null, refreshing: true }).wrapper.text()).toContain('Loading streams')
    const unavailable = open({ catalog: null, catalogError: 'Unable to load RTMP state' }).wrapper
    expect(unavailable.get('[role="alert"]').text()).toContain('Streams unavailable.')
  })
})

describe('recording control', () => {
  it('starts a manual recorder with the active generation revision and keeps the result visible', async () => {
    const { calls, mutations } = install()
    const { wrapper, reload } = open()
    await tile(wrapper, 'cam-front').get('[data-recorder="archive"] button').trigger('click')
    await flushPromises()

    expect(mutations()).toHaveLength(1)
    expect(mutations()[0]).toMatchObject({
      url: '/api/v1/rtmp/streams/stream-front/recorders/rec-archive/start',
      headers: { Authorization: 'Bearer test-token', 'If-Generation-Revision': 'active-revision' },
    })
    expect(calls[0]!.url).toBe('/api/v1/generations')
    expect(wrapper.get('[data-feedback] [role="status"]').text()).toContain('Recording live / cam-front with archive was requested.')
    expect(toasts.value.map(({ text }) => text)).toEqual(['Recording live / cam-front with archive was requested.'])
    expect(reload).toHaveBeenCalledTimes(1)
  })

  it('offers no command on an automatic recorder and says what it writes', () => {
    install()
    const { wrapper } = open()
    const continuous = tile(wrapper, 'cam-front').get('[data-recorder="continuous"]')
    expect(continuous.findAll('button')).toHaveLength(0)
    expect(continuous.text()).toContain('automatic')
    expect(continuous.text()).toContain('RECORDING')
    expect(continuous.text()).toContain('cam-front-2026-01-01T10-00-00.flv')
  })

  it('names the stream and recorder when stopping and sends nothing when declined', async () => {
    const { wrapper } = open()
    const net = install()
    const catalog = camerasCatalog(NOW)
    catalog.streams[0]!.recorders[0]!.phase = { state: 'recording', operation_id: 'op', started_at_unix_ms: NOW - 1000 }
    await wrapper.setProps({ catalog })
    await tile(wrapper, 'cam-front').get('[data-recorder="archive"] button').trigger('click')
    const asked = await answer(false)
    expect(asked?.title).toBe('Stop recording live / cam-front')
    expect(asked?.message).toContain('archive')
    expect(net.mutations()).toHaveLength(0)
  })

  it('reports a refused command in words and keeps it until dismissed', async () => {
    install((call) => call.url.includes('/recorders/')
      ? Response.json({ error: { code: 'rtmp_state_conflict', message: 'The recorder is already starting.' } }, { status: 409 })
      : undefined)
    const { wrapper, reload } = open()
    await tile(wrapper, 'cam-front').get('[data-recorder="archive"] button').trigger('click')
    await flushPromises()
    const alert = wrapper.get('[data-feedback] [role="alert"]')
    expect(alert.text()).toContain('Could not start recording live / cam-front: The recorder is already starting.')
    expect(alert.text()).toContain('The stream changed while the command was in flight')
    expect(reload).toHaveBeenCalledTimes(1)
    await alert.get('button').trigger('click')
    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toBe('')
  })
})

describe('publisher and viewer control', () => {
  it('names the publisher, its address and the audience before dropping it', async () => {
    const net = install()
    const { wrapper, reload } = open()
    await tile(wrapper, 'cam-front').get('[aria-label="Drop publisher of live / cam-front"]').trigger('click')
    const asked = await answer(true)

    expect(asked).toMatchObject({ title: 'Drop the publisher of live / cam-front', danger: true, confirmLabel: 'Drop publisher' })
    expect(asked?.details).toEqual(['Publisher: 192.0.2.10', 'Connected for 1h 0m', '3 viewers will lose the stream'])
    expect(net.mutations()).toEqual([expect.objectContaining({
      url: '/api/v1/rtmp/clients/sess-front-pub/publisher/drop',
      headers: expect.objectContaining({ 'If-Rtmp-Session-Revision': '11' }),
    })])
    expect(wrapper.get('[data-feedback] [role="status"]').text()).toContain('The publisher of live / cam-front was asked to disconnect.')
    expect(reload).toHaveBeenCalledTimes(1)
  })

  it('sends nothing when the confirmation is cancelled', async () => {
    const net = install()
    const { wrapper } = open()
    await tile(wrapper, 'cam-garage').get('[aria-label="Drop publisher of live / cam-garage"]').trigger('click')
    await answer(false)
    expect(net.mutations()).toHaveLength(0)
  })

  it('cannot drop a publisher that is not in the client list yet and says why', () => {
    install()
    const { wrapper } = open({ stats: null })
    const front = tile(wrapper, 'cam-front')
    expect(front.get('[aria-label="Drop publisher of live / cam-front"]').attributes('disabled')).toBeDefined()
    expect(front.text()).toContain('not in the client list yet')
  })

  it('lists the viewers on demand and drops one by name after a confirmation', async () => {
    const net = install()
    const { wrapper } = open()
    const front = tile(wrapper, 'cam-front')
    expect(front.find('[aria-label="Viewers of live / cam-front"]').exists()).toBe(false)
    await front.get('.ox-toggle').trigger('click')
    expect(front.get('[aria-label="Viewers of live / cam-front"]').text()).toContain('198.51.100.22')

    await front.get('[aria-label="Drop viewer 198.51.100.22 of live / cam-front"]').trigger('click')
    const asked = await answer(true)
    expect(asked?.title).toBe('Drop the viewer 198.51.100.22')
    expect(asked?.details).toEqual(['Stream: live / cam-front', 'Session: sess-view-2'])
    expect(net.mutations()[0]!.url).toBe('/api/v1/rtmp/clients/sess-view-2/subscriber/drop')
    expect(wrapper.get('[data-feedback] [role="status"]').text()).toContain('The viewer 198.51.100.22 of live / cam-front was asked to disconnect.')
  })

  it('says so when a drop was already requested', async () => {
    install((call) => call.url.includes('/drop')
      ? Response.json({ outcome: 'already_requested', sessionId: 'sess-front-pub', target: 'publisher', sessionRevision: '11' }, { status: 202 })
      : undefined)
    const { wrapper } = open()
    await tile(wrapper, 'cam-front').get('[aria-label="Drop publisher of live / cam-front"]').trigger('click')
    await answer(true)
    expect(wrapper.get('[data-feedback] [role="status"]').text()).toContain('Dropping The publisher of live / cam-front was already requested.')
  })

  it('signs out on a rejected token and still reports the failure', async () => {
    install((call) => call.url.includes('/drop')
      ? Response.json({ error: { code: 'unauthorized', message: 'invalid bearer token' } }, { status: 401 })
      : undefined)
    const { wrapper } = open()
    await tile(wrapper, 'cam-front').get('[aria-label="Drop publisher of live / cam-front"]').trigger('click')
    await answer(true)
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toContain('invalid bearer token')
  })

  it('drives the shared confirmation dialog with the target in its heading', async () => {
    install()
    const host = mount(ConfirmHost)
    const { wrapper } = open()
    await tile(wrapper, 'cam-front').get('[aria-label="Drop publisher of live / cam-front"]').trigger('click')
    await flushPromises()
    expect(host.get('h2').text()).toBe('Drop the publisher of live / cam-front')
    expect(host.text()).toContain('3 viewers will lose the stream')
  })
})

describe('live preview', () => {
  const startPreview = async (wrapper: VaporWrapper, name = 'cam-front') => {
    await tile(wrapper, name).get(`[aria-label="Preview live / ${name}"]`).trigger('click')
    await flushPromises()
  }

  it('mints a ticket for the stream and plays the ticketed playlist in a labelled video', async () => {
    const net = install()
    const { wrapper } = open()
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(false)
    await startPreview(wrapper)

    expect(net.mutations()).toEqual([expect.objectContaining({
      url: '/api/v1/rtmp/media/edge/live/cam-front/ticket',
      body: { ttlSeconds: 600 },
    })])
    const video = tile(wrapper, 'cam-front').get('video')
    expect(video.attributes('aria-label')).toBe('Live preview of live / cam-front')
    expect(video.attributes('src')).toBe(`/api/v1/rtmp/media/edge/live/cam-front/index.m3u8?ticket=${TICKET}`)
    expect(wrapper.get('[aria-label="Stop preview of live / cam-front"]').attributes('aria-pressed')).toBe('true')
  })

  it('keeps the ticket out of every text, toast and label', async () => {
    install()
    const { wrapper } = open()
    await startPreview(wrapper)
    expect(wrapper.text()).not.toContain(TICKET)
    expect(toasts.value).toHaveLength(0)
    const holders = [...document.querySelectorAll('*'), ...wrapper.element.querySelectorAll('*')]
      .filter((element) => [...element.attributes].some(({ name, value }) => value.includes(TICKET) && name !== 'src'))
    expect(holders).toEqual([])
  })

  it('stops the preview and drops the video', async () => {
    install()
    const { wrapper } = open()
    await startPreview(wrapper)
    await wrapper.get('[aria-label="Stop preview of live / cam-front"]').trigger('click')
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(false)
  })

  it('says that an application without a media output cannot be previewed', async () => {
    install((call) => call.url.endsWith('/ticket')
      ? Response.json({ error: { code: 'media_not_found', message: 'the application has no HLS or DASH output' } }, { status: 404 })
      : undefined)
    const { wrapper } = open()
    await startPreview(wrapper)
    expect(tile(wrapper, 'cam-front').text()).toContain('This application has no HLS output')
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(false)
  })

  it('says that a DASH-only application cannot be previewed natively', async () => {
    install((call) => call.url.endsWith('/ticket')
      ? Response.json({ ticket: TICKET, path: `/api/v1/rtmp/media/edge/live/cam-front/manifest.mpd?ticket=${TICKET}`, expiresAtUnixMs: NOW + 600_000, ttlSeconds: 600 })
      : undefined)
    const { wrapper } = open()
    await startPreview(wrapper)
    expect(tile(wrapper, 'cam-front').text()).toContain('only publishes DASH')
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(false)
  })

  it('offers a retry after a playback error and mints a new ticket', async () => {
    const net = install()
    const { wrapper } = open()
    await startPreview(wrapper)
    await tile(wrapper, 'cam-front').get('video').trigger('error')
    expect(tile(wrapper, 'cam-front').text()).toContain('The preview could not be played.')
    await tile(wrapper, 'cam-front').get('.ox-cams-preview button').trigger('click')
    await flushPromises()
    expect(net.mutations()).toHaveLength(2)
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(true)
  })

  it('renews the ticket thirty seconds before its lifetime ends without leaving the video', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const net = install()
    const { wrapper } = open()
    await startPreview(wrapper)
    expect(net.mutations()).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(600_000 - 30_000 - 1)
    expect(net.mutations()).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(2)
    await flushPromises()
    expect(net.mutations()).toHaveLength(2)
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(true)
  })

  it('renews at half the lifetime of a short ticket whatever the clock says', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const net = install((call) => call.url.endsWith('/ticket')
      ? Response.json({ ticket: TICKET, path: `/api/v1/rtmp/media/edge/live/cam-front/index.m3u8?ticket=${TICKET}`, expiresAtUnixMs: 1, ttlSeconds: 20 })
      : undefined)
    const { wrapper } = open()
    await startPreview(wrapper)
    await vi.advanceTimersByTimeAsync(9_999)
    expect(net.mutations()).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(2)
    expect(net.mutations()).toHaveLength(2)
  })

  it('stops renewing once the tile is gone', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const net = install()
    const { wrapper } = open()
    await startPreview(wrapper)
    wrapper.unmount()
    await vi.advanceTimersByTimeAsync(900_000)
    expect(net.mutations()).toHaveLength(1)
  })

  it('plays at most four previews and drops the oldest', async () => {
    const net = install()
    const catalog = camerasCatalog(NOW)
    const extra = ['a', 'b', 'c'].map((suffix) => ({
      ...catalog.streams[0]!, id: `stream-extra-${suffix}`, name: `cam-extra-${suffix}`,
      publisher: { session_id: `sess-extra-${suffix}`, attached_at_unix_ms: NOW },
    }))
    catalog.streams.push(...extra)
    const { wrapper } = open({ catalog })
    for (const name of ['cam-front', 'cam-garage', 'cam-side', 'cam-extra-a', 'cam-extra-b']) await startPreview(wrapper, name)
    expect(wrapper.findAll('video')).toHaveLength(4)
    expect(tile(wrapper, 'cam-front').find('video').exists()).toBe(false)
    expect(net.mutations()).toHaveLength(5)
  })

  it('cannot preview in a browser without native HLS and says so', () => {
    vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockReturnValue('')
    install()
    const { wrapper } = open()
    expect(tile(wrapper, 'cam-front').get('[aria-label="Preview live / cam-front"]').attributes('disabled')).toBeDefined()
    expect(tile(wrapper, 'cam-front').text()).toContain('This browser cannot play HLS')
  })

  it('hands a rejected token to the shell', async () => {
    install((call) => call.url.endsWith('/ticket')
      ? Response.json({ error: { code: 'unauthorized', message: 'invalid bearer token' } }, { status: 401 })
      : undefined)
    const { wrapper } = open()
    await startPreview(wrapper)
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
  })
})
