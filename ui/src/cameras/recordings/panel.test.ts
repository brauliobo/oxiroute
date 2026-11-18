import { afterEach, describe, expect, it, vi } from 'vitest'
import '../../test/dialog'
import { clearToasts, toasts } from '#design/toast/state'
import { recording, recordingPage, recordingRoots } from '../../test/camerasFixtures'
import { jsonResponse } from '../../test/contractFixtures'
import { flushPromises, mount, type VaporWrapper } from '../../test/mount'
import { pendingConfirm, settleConfirm } from '../../shell/confirm'
import type { RecordingPage } from '../../api'
import RecordingsPanel from './RecordingsPanel.vue'

const token = 'recordings-test-token'
const TICKET = 'tk-9f3a1c77d2'
const BASE = '/api/v1/rtmp/recordings'

interface Call {
  url: string
  method: string
  path: string
  query: URLSearchParams
}

type Reply = (call: Call) => Response | undefined

afterEach(() => {
  settleConfirm(false)
  clearToasts()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

function stubNetwork(reply: Reply = () => undefined, page: () => RecordingPage = () => recordingPage()) {
  const calls: Call[] = []
  const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const [path = '', search = ''] = String(input).split('?')
    const call = { url: String(input), method: init?.method ?? 'GET', path, query: new URLSearchParams(search) }
    calls.push(call)
    return Promise.resolve(reply(call) ?? defaultReply(call, page))
  })
  vi.stubGlobal('fetch', fetch)
  const lists = () => calls.filter((call) => call.method === 'GET')
  const mutations = () => calls.filter((call) => call.method !== 'GET')
  return { calls, lists, mutations }
}

function defaultReply(call: Call, page: () => RecordingPage): Response {
  if (call.method === 'GET') return jsonResponse(page())
  const id = decodeURIComponent(call.path.split('/')[5] ?? '')
  if (call.method === 'DELETE') return jsonResponse({ deleted: page().recordings.find((item) => item.id === id) ?? recording(0) })
  return jsonResponse({
    ticket: TICKET,
    path: `${BASE}/${id}/media?ticket=${TICKET}`,
    expiresAtUnixMs: 1_900_000_000_000,
    ttlSeconds: 60,
  })
}

async function open(reply?: Reply, page?: () => RecordingPage) {
  const net = stubNetwork(reply, page)
  const wrapper = mount(RecordingsPanel, { props: { token } })
  await flushPromises()
  return { wrapper, ...net }
}

const rowOf = (wrapper: VaporWrapper, id: string) => wrapper.get(`[data-recording-id="${id}"]`)
const rowNames = (wrapper: VaporWrapper) => wrapper.findAll('tbody tr').map((row) => row.get('th').text().slice(-12, -4))
const idOf = (index: number) => recording(index).id

function control(wrapper: VaporWrapper, label: string) {
  const field = wrapper.findAll('.ox-field').find((candidate) => candidate.get('label').text() === label)
  return field!.get('select, input')
}

async function choose(wrapper: VaporWrapper, label: string, value: string) {
  await control(wrapper, label).setValue(value)
  await flushPromises()
}

async function answer(confirmed: boolean) {
  await flushPromises()
  const asked = pendingConfirm.value
  settleConfirm(confirmed)
  await flushPromises()
  return asked
}

describe('first load', () => {
  it('asks for the first page of fifty without filters and lists every recording', async () => {
    const { wrapper, lists } = await open()

    expect(lists()).toHaveLength(1)
    expect(lists()[0]!.path).toBe(BASE)
    expect(lists()[0]!.query.get('limit')).toBe('50')
    expect(lists()[0]!.query.has('stream')).toBe(false)
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
    expect(wrapper.get('[data-list-summary]').text()).toBe('Showing 4 of 4 recordings. Sorted among the 4 loaded recordings.')
    expect(wrapper.text()).toContain('Browsers cannot play FLV')
    expect(wrapper.text()).not.toContain('Play')
  })

  it('shows loading until the first page arrives', async () => {
    stubNetwork(() => new Promise<Response>(() => undefined) as unknown as Response)
    const wrapper = mount(RecordingsPanel, { props: { token } })
    await flushPromises()
    expect(wrapper.get('.ox-loading').text()).toBe('Loading recordings')
    expect(wrapper.find('table').exists()).toBe(false)
  })

  it('requests nothing without a token', async () => {
    const net = stubNetwork()
    mount(RecordingsPanel, { props: { token: '' } })
    await flushPromises()
    expect(net.calls).toHaveLength(0)
  })

  it('describes each recording with its stream, start, duration, size and codecs', async () => {
    const { wrapper } = await open()
    const row = rowOf(wrapper, idOf(2)).text()

    expect(row).toContain('cam-front-2026-01-01T12-00-00.flv')
    expect(row).toContain('cam-front')
    expect(row).toContain('2026-01-01 12:00:00 UTC')
    expect(row).toContain('30m 0s')
    expect(row).toContain('450 MiB')
    expect(row).toContain('avc / aac')
    expect(rowOf(wrapper, idOf(0)).text()).toContain('—')
    expect(rowOf(wrapper, idOf(0)).text()).toContain('avc / no audio')
  })
})

describe('status words', () => {
  it('shows complete, partial and recording as words with their reasons', async () => {
    const { wrapper } = await open()

    expect(rowOf(wrapper, idOf(2)).get('[data-status]').text()).toBe('COMPLETE')
    expect(rowOf(wrapper, idOf(0)).get('[data-status]').text()).toContain('PARTIAL')
    expect(rowOf(wrapper, idOf(0)).get('[data-status]').text()).toContain('The file ends at the last whole frame.')
    expect(rowOf(wrapper, idOf(3)).get('[data-status]').text()).toContain('RECORDING')
    expect(rowOf(wrapper, idOf(3)).get('[data-status]').text()).toContain('Being written now.')
  })

  it('disables Delete for a recording being written and says why', async () => {
    const { wrapper } = await open()
    const row = rowOf(wrapper, idOf(3))
    const remove = row.get('[data-action="delete"]')

    expect(remove.attributes('disabled')).toBeDefined()
    const reason = wrapper.get(`[id="${idOf(3)}-reason"]`)
    expect(remove.attributes('aria-describedby')).toBe(reason.attributes('id'))
    expect(reason.text()).toContain('cannot be deleted until its recorder stops')
    expect(rowOf(wrapper, idOf(2)).get('[data-action="delete"]').attributes('disabled')).toBeUndefined()
  })

  it('gives every action an accessible name with its visible word and the file name', async () => {
    const { wrapper } = await open()
    const row = rowOf(wrapper, idOf(2))
    const name = 'cam-front-2026-01-01T12-00-00.flv'

    expect(row.get('[data-action="download"]').attributes('aria-label')).toBe(`Download ${name}`)
    expect(row.get('[data-action="download"]').text()).toBe('Download')
    expect(row.get('[data-action="delete"]').attributes('aria-label')).toBe(`Delete ${name}`)
    expect(row.get('[data-action="delete"]').text()).toBe('Delete')
  })
})

describe('quota and retention', () => {
  it('shows a card per root with meters, retention words and evictions', async () => {
    const { wrapper } = await open()
    const cards = wrapper.findAll('[data-root]')
    const archive = wrapper.get('[data-root="live/archive"]')

    expect(cards.map((card) => card.attributes('data-root'))).toEqual(['live/archive', 'archive/cold'])
    const bytes = archive.get('[data-usage="bytes"] [role="meter"]')
    expect(bytes.attributes('aria-valuetext')).toBe('7 GiB of 10 GiB (70%)')
    expect(bytes.classes()).toContain('ox-tone-ok')
    expect(archive.get('[data-usage="files"] [role="meter"]').attributes('aria-valuetext')).toBe('4 of 10 (40%)')
    expect(archive.text()).toContain('the oldest finished recordings are deleted to make room (delete_oldest)')
    expect(archive.text()).toContain('older than 7d 0h are deleted')
    expect(archive.text()).toContain('Evicted so far: 3 files, 1.5 GiB.')
    expect(wrapper.get('[data-root="archive/cold"]').text()).toContain('the recorder stops with the failure code quota_files or quota_bytes')
    expect(wrapper.get('[data-root="archive/cold"] [data-usage="bytes"] [role="meter"]').classes()).toContain('ox-tone-bad')
  })

  it('shows no meter but no limit when a maximum is not set', async () => {
    const { wrapper } = await open(undefined, () => {
      const [first, second] = recordingRoots()
      return recordingPage({ roots: [{ ...first!, max_bytes: null, max_files: null }, second!] })
    })
    const archive = wrapper.get('[data-root="live/archive"]')

    expect(archive.find('[role="meter"]').exists()).toBe(false)
    expect(archive.text()).toContain('No limit')
  })

  it('warns that a full reject root will fail its recorder and what to do', async () => {
    const { wrapper } = await open()
    const alert = wrapper.get('[data-quota-alert="archive/cold"]')

    expect(alert.attributes('data-tone')).toBe('bad')
    expect(alert.text()).toContain('New recordings are refused')
    expect(alert.text()).toContain('quota_files')
    expect(alert.text()).toContain('Delete recordings or raise the limit in the recorder configuration.')
    expect(wrapper.find('[data-quota-alert="live/archive"]').exists()).toBe(false)
  })

  it('stays quiet while a reject root is below 90 percent', async () => {
    const { wrapper } = await open(undefined, () => {
      const [first, second] = recordingRoots()
      return recordingPage({ roots: [first!, { ...second!, bytes_used: String(5 * 1024 ** 3), files: 5 }] })
    })
    expect(wrapper.find('[data-quota-alert]').exists()).toBe(false)
  })
})

describe('filters', () => {
  it('reloads from the first page with the stream sent to the server', async () => {
    const { wrapper, lists } = await open()
    expect(control(wrapper, 'Stream').findAll('option').map((option) => option.text())).toEqual(['All streams', 'cam-front', 'cam-garage'])

    await choose(wrapper, 'Stream', 'cam-garage')

    expect(lists()).toHaveLength(2)
    expect(lists()[1]!.query.get('stream')).toBe('cam-garage')
    expect(lists()[1]!.query.get('limit')).toBe('50')
    expect(lists()[1]!.query.has('cursor')).toBe(false)
  })

  it('sends the status to the server', async () => {
    const { wrapper, lists } = await open()
    await choose(wrapper, 'Status', 'partial')
    expect(lists()[1]!.query.get('status')).toBe('partial')
  })

  it('keeps the other streams selectable after the list is narrowed to one', async () => {
    const { wrapper } = await open(
      (call) => (call.query.get('stream') === 'cam-garage' ? jsonResponse(recordingPage({ recordings: [recording(1, { stream: 'cam-garage' })] })) : undefined),
    )
    await choose(wrapper, 'Stream', 'cam-garage')

    expect(wrapper.findAll('tbody tr')).toHaveLength(1)
    expect(control(wrapper, 'Stream').findAll('option').map((option) => option.text())).toEqual(['All streams', 'cam-front', 'cam-garage'])
  })

  it('filters by codec, name and UTC day without asking the server again', async () => {
    const { wrapper, lists } = await open()

    await choose(wrapper, 'Video codec', 'hevc')
    expect(rowNames(wrapper)).toEqual(['11-00-00'])
    await choose(wrapper, 'Video codec', '')
    await choose(wrapper, 'Search by name', 'T12-')
    expect(rowNames(wrapper)).toEqual(['12-00-00'])
    await choose(wrapper, 'Search by name', '')
    await choose(wrapper, 'From (UTC day)', '2026-01-01')
    await choose(wrapper, 'To (UTC day, inclusive)', '2026-01-01')
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
    await choose(wrapper, 'To (UTC day, inclusive)', '2025-12-31')
    expect(wrapper.text()).toContain('No recording matches the filters')
    expect(lists()).toHaveLength(1)
  })

  it('says no recording matches and clears the filters from there', async () => {
    const { wrapper } = await open()
    await choose(wrapper, 'Search by name', 'nothing like this')

    expect(wrapper.get('.ox-empty').text()).toContain('No recording matches the filters')
    expect(wrapper.find('table').exists()).toBe(false)
    await wrapper.get('[data-action="clear-filters-empty"]').trigger('click')
    await flushPromises()
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
    expect(control(wrapper, 'Search by name').element).toHaveProperty('value', '')
  })

  it('clears the server filters with a reload', async () => {
    const { wrapper, lists } = await open()
    await choose(wrapper, 'Status', 'partial')
    await wrapper.get('[data-action="clear-filters"]').trigger('click')
    await flushPromises()

    expect(lists()).toHaveLength(3)
    expect(lists()[2]!.query.has('status')).toBe(false)
  })

  it('explains an empty server instead of a filtered empty list', async () => {
    const { wrapper } = await open(undefined, () => recordingPage({ recordings: [] }))

    expect(wrapper.get('.ox-empty').text()).toContain('No recordings yet')
    expect(wrapper.text()).toContain('Recordings appear after a recorder with a root directory has written a file.')
    expect(wrapper.find('[data-action="clear-filters"]').exists()).toBe(false)
  })

  it('shows the filtered empty state with its own Clear filters button when the server filter matches nothing', async () => {
    const { wrapper } = await open((call) => (call.query.has('status') ? jsonResponse(recordingPage({ recordings: [] })) : undefined))
    await choose(wrapper, 'Status', 'complete')

    expect(wrapper.get('.ox-empty').text()).toContain('No recording matches the filters')
    await wrapper.get('[data-action="clear-filters-empty"]').trigger('click')
    await flushPromises()
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
  })
})

describe('pagination', () => {
  const second = [recording(7, { name: 'later-a.flv' }), recording(8, { name: 'later-b.flv' })]

  it('appends the next page with its cursor and says more are available until the last one', async () => {
    const { wrapper, lists } = await open(
      (call) => (call.query.get('cursor') === 'page-2' ? jsonResponse(recordingPage({ recordings: second })) : undefined),
      () => recordingPage({ next_cursor: 'page-2' }),
    )
    expect(wrapper.get('[data-list-summary]').text()).toContain('Showing 4 of 4 loaded recordings; more are available.')

    await wrapper.get('[data-action="load-more"]').trigger('click')
    await flushPromises()

    expect(lists()[1]!.query.get('cursor')).toBe('page-2')
    expect(lists()[1]!.query.get('limit')).toBe('50')
    expect(wrapper.findAll('tbody tr')).toHaveLength(6)
    expect(wrapper.get('[data-list-summary]').text()).toContain('Showing 6 of 6 recordings.')
    expect(wrapper.find('[data-action="load-more"]').exists()).toBe(false)
  })

  it('keeps the filters when it loads more', async () => {
    const { wrapper, lists } = await open(undefined, () => recordingPage({ next_cursor: 'page-2' }))
    await choose(wrapper, 'Status', 'partial')
    await wrapper.get('[data-action="load-more"]').trigger('click')
    await flushPromises()

    expect(lists()[2]!.query.get('status')).toBe('partial')
    expect(lists()[2]!.query.get('cursor')).toBe('page-2')
  })

  it('offers no Load more on the last page', async () => {
    const { wrapper } = await open()
    expect(wrapper.find('[data-action="load-more"]').exists()).toBe(false)
  })
})

describe('sorting', () => {
  it('starts with the newest start first and marks it for assistive technology', async () => {
    const { wrapper } = await open()
    const heads = wrapper.findAll('thead th')

    expect(rowNames(wrapper)).toEqual(['13-00-00', '12-00-00', '11-00-00', '10-00-00'])
    expect(heads.map((head) => head.attributes('aria-sort') ?? null)).toEqual(
      ['none', 'none', 'descending', 'none', 'none', 'none', null, null],
    )
  })

  it('changes the order and aria-sort when a column is chosen, and reverses on a second click', async () => {
    const { wrapper } = await open()
    const head = (label: string) => wrapper.findAll('thead th').find((cell) => cell.text().startsWith(label))!

    await head('Size').get('button').trigger('click')
    expect(rowNames(wrapper)).toEqual(['11-00-00', '13-00-00', '12-00-00', '10-00-00'])
    expect(head('Size').attributes('aria-sort')).toBe('descending')
    expect(head('Started').attributes('aria-sort')).toBe('none')

    await head('Size').get('button').trigger('click')
    expect(head('Size').attributes('aria-sort')).toBe('ascending')
    expect(rowNames(wrapper)[3]).toBe('11-00-00')
  })

  it('offers the sort as a choice for narrow screens', async () => {
    const { wrapper } = await open()
    await choose(wrapper, 'Sort by', 'name:ascending')
    expect(rowNames(wrapper)).toEqual(['10-00-00', '11-00-00', '12-00-00', '13-00-00'])
  })
})

describe('download', () => {
  function captureLinks() {
    const links: { href: string | null; name: string | null }[] = []
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function click(this: HTMLAnchorElement) {
      links.push({ href: this.getAttribute('href'), name: this.getAttribute('download') })
    })
    return links
  }

  it('mints a ticket and navigates a temporary link to its path without reloading the list', async () => {
    const links = captureLinks()
    const { wrapper, calls, lists, mutations } = await open()

    await rowOf(wrapper, idOf(2)).get('[data-action="download"]').trigger('click')
    await flushPromises()

    expect(mutations().map((call) => [call.method, call.path])).toEqual([['POST', `${BASE}/${idOf(2)}/ticket`]])
    expect(links).toEqual([{ href: `${BASE}/${idOf(2)}/media?ticket=${TICKET}`, name: 'cam-front-2026-01-01T12-00-00.flv' }])
    expect(document.querySelector('a[download]')).toBeNull()
    expect(lists()).toHaveLength(1)
    expect(calls).toHaveLength(2)
  })

  it('reports the start of the download without the ticket anywhere on the page', async () => {
    captureLinks()
    const { wrapper } = await open()
    await rowOf(wrapper, idOf(2)).get('[data-action="download"]').trigger('click')
    await flushPromises()

    const feedback = wrapper.get('[data-feedback]')
    expect(feedback.text()).toContain('Download of cam-front-2026-01-01T12-00-00.flv started.')
    expect(feedback.text()).toContain('Open it in a media player')
    expect(toasts.value.map((toast) => toast.text)).toEqual(['Download of cam-front-2026-01-01T12-00-00.flv started.'])
    expect(wrapper.text()).not.toContain(TICKET)
    expect(document.body.innerHTML).not.toContain(TICKET)
    expect(JSON.stringify(toasts.value)).not.toContain(TICKET)
  })

  it('says the recording is gone and reloads the list when it no longer exists', async () => {
    const links = captureLinks()
    const { wrapper, lists } = await open((call) => (
      call.method === 'POST' ? jsonResponse({ error: { code: 'recording_not_found', message: 'no such recording' } }, 404) : undefined
    ))
    await rowOf(wrapper, idOf(2)).get('[data-action="download"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toContain(
      'Could not download cam-front-2026-01-01T12-00-00.flv: The recording no longer exists; the list was reloaded.',
    )
    expect(links).toEqual([])
    expect(lists()).toHaveLength(2)
  })

  it('reports another failure in words and keeps the list', async () => {
    captureLinks()
    const { wrapper, lists } = await open((call) => (
      call.method === 'POST' ? jsonResponse({ error: { code: 'ticket_unavailable', message: 'tickets are disabled' } }, 503) : undefined
    ))
    await rowOf(wrapper, idOf(2)).get('[data-action="download"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toContain('Could not download')
    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toContain('tickets are disabled')
    expect(lists()).toHaveLength(1)
  })

  it('tells the console when the management token is rejected', async () => {
    captureLinks()
    const { wrapper } = await open((call) => (
      call.method === 'POST' ? jsonResponse({ error: { code: 'unauthorized', message: 'invalid bearer token' } }, 401) : undefined
    ))
    await rowOf(wrapper, idOf(2)).get('[data-action="download"]').trigger('click')
    await flushPromises()
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
  })
})

describe('delete', () => {
  it('asks in the shared dialog naming the recording, the consequences and the details', async () => {
    const { wrapper, mutations } = await open()
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    const asked = await answer(false)

    expect(asked).toMatchObject({ title: 'Delete cam-front-2026-01-01T12-00-00.flv', danger: true, confirmLabel: 'Delete recording' })
    expect(asked!.message).toContain('cannot be undone')
    expect(asked!.message).toContain('metadata sidecar')
    expect(asked!.details).toEqual(['Stream: cam-front', 'Size: 450 MiB', 'Started: 2026-01-01 12:00:00 UTC'])
    expect(mutations()).toHaveLength(0)
  })

  it('sends nothing when the dialog is cancelled', async () => {
    const { wrapper, calls } = await open()
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    await answer(false)

    expect(calls).toHaveLength(1)
    expect(wrapper.get('[data-feedback]').text()).toBe('')
  })

  it('deletes after the confirmation, reloads from the first page keeping the filters and reports it', async () => {
    const { wrapper, lists, mutations } = await open()
    await choose(wrapper, 'Status', 'complete')
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    await answer(true)

    expect(mutations().map((call) => [call.method, call.path])).toEqual([['DELETE', `${BASE}/${idOf(2)}`]])
    expect(lists()).toHaveLength(3)
    expect(lists()[2]!.query.get('status')).toBe('complete')
    expect(lists()[2]!.query.has('cursor')).toBe(false)
    expect(wrapper.get('[data-feedback]').text()).toContain('Deleted cam-front-2026-01-01T12-00-00.flv and its metadata sidecar.')
    expect(toasts.value[0]!.tone).toBe('ok')
  })

  it('explains in words that a recording still being written cannot be deleted, then reloads', async () => {
    const { wrapper, lists } = await open((call) => (
      call.method === 'DELETE' ? jsonResponse({ error: { code: 'recording_active', message: 'recording is still being written' } }, 409) : undefined
    ))
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    await answer(true)

    const alert = wrapper.get('[data-feedback] [role="alert"]').text()
    expect(alert).toContain('Could not delete cam-front-2026-01-01T12-00-00.flv: recording is still being written.')
    expect(alert).toContain('cannot be deleted until its recorder stops')
    expect(lists()).toHaveLength(2)
  })

  it('reloads and says so when the recording was already deleted elsewhere', async () => {
    const { wrapper, lists } = await open((call) => (
      call.method === 'DELETE' ? jsonResponse({ error: { code: 'recording_not_found', message: 'gone' } }, 404) : undefined
    ))
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    await answer(true)

    expect(wrapper.get('[data-feedback] [role="alert"]').text()).toContain('The recording no longer exists; the list was reloaded.')
    expect(lists()).toHaveLength(2)
  })

  it('keeps the outcome visible after the reload', async () => {
    const { wrapper } = await open()
    await rowOf(wrapper, idOf(2)).get('[data-action="delete"]').trigger('click')
    await answer(true)
    await wrapper.get('[data-action="refresh-recordings"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-feedback]').text()).toContain('Deleted')
  })
})

describe('failures', () => {
  it('shows an alert with Retry and keeps the last good list when a refresh fails', async () => {
    let failing = false
    const { wrapper, lists } = await open(() => (
      failing ? jsonResponse({ error: { code: 'internal', message: 'recording index unavailable' } }, 500) : undefined
    ))

    failing = true
    await wrapper.get('[data-action="refresh-recordings"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('[data-recordings-error]').text()).toContain('Recordings unavailable.')
    expect(wrapper.get('[data-recordings-error]').text()).toContain('recording index unavailable')
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
    expect(wrapper.find('[data-root]').exists()).toBe(true)

    failing = false
    await wrapper.get('[data-action="retry-recordings"]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-recordings-error]').exists()).toBe(false)
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
    expect(lists()).toHaveLength(3)
  })

  it('offers Retry when the first load fails and shows no placeholder data', async () => {
    let failing = true
    const { wrapper } = await open(() => (
      failing ? jsonResponse({ error: { code: 'internal', message: 'not ready' } }, 500) : undefined
    ))
    expect(wrapper.get('[data-recordings-error]').text()).toContain('not ready')
    expect(wrapper.find('table').exists()).toBe(false)

    failing = false
    await wrapper.get('[data-action="retry-recordings"]').trigger('click')
    await flushPromises()
    expect(wrapper.findAll('tbody tr')).toHaveLength(4)
  })

  it('emits unauthorized for a rejected token', async () => {
    const { wrapper } = await open(() => jsonResponse({ error: { code: 'unauthorized', message: 'invalid bearer token' } }, 401))
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
    expect(wrapper.get('[data-recordings-error]').text()).toContain('invalid bearer token')
  })

  it('reloads from the first page on Refresh', async () => {
    const { wrapper, lists } = await open()
    await wrapper.get('[data-action="refresh-recordings"]').trigger('click')
    await flushPromises()
    expect(lists()).toHaveLength(2)
  })
})
