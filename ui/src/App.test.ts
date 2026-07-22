import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'

const catalog = {
  revision: '4',
  as_of_unix_ms: 1_750_000_000_000,
  capabilities: {
    live_ingest: true,
    manual_recording: true,
  },
  streams: [
    {
      id: '2a130dea-5db7-43e0-afb8-f07c4bcb1814',
      revision: '3',
      server_id: 'edge',
      application: 'live',
      name: 'camera',
      created_at_unix_ms: 1_750_000_000_000,
      publisher: {
        session_id: '750a865d-1b72-4a5f-a54b-a1d8510d055c',
        attached_at_unix_ms: 1_750_000_000_000,
      },
      subscriber_count: 12,
      media: {
        audio: {
          codec_id: 10,
          codec_name: 'aac',
          payload_bytes: '1024',
          last_rtmp_timestamp_ms: 120,
          last_observed_at_unix_ms: 1_750_000_000_200,
        },
        video: {
          codec_id: 7,
          codec_name: 'avc',
          payload_bytes: '4096',
          last_rtmp_timestamp_ms: 123,
          last_observed_at_unix_ms: 1_750_000_000_200,
        },
        fanout_payload_bytes: '8192',
      },
      recorders: [
        {
          id: 'c76ad8c2-e575-4989-8fae-1a95566ff598',
          name: 'archive',
          manual: true,
          phase: { state: 'idle' },
          changed_at_unix_ms: 1_750_000_000_000,
          bytes_written: '0',
        },
      ],
    },
  ],
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('RTMP broadcast desk', () => {
  it('shows active streams and sends recorder controls', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(catalog), { status: 200 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            ...catalog.streams[0]!.recorders[0]!,
            phase: { state: 'starting', operation_id: 'operation-1' },
          }),
          { status: 202 },
        ),
      )
      .mockResolvedValueOnce(new Response(JSON.stringify(catalog), { status: 200 }))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.text()).toContain('live / camera')
    expect(wrapper.text()).toContain('12 viewers')
    expect(wrapper.text()).toContain('AAC')
    expect(wrapper.text()).toContain('AVC')
    await wrapper.get('[data-recorder-action]').trigger('click')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/rtmp/streams/2a130dea-5db7-43e0-afb8-f07c4bcb1814/recorders/c76ad8c2-e575-4989-8fae-1a95566ff598/start',
      expect.objectContaining({ method: 'POST' }),
    )
    wrapper.unmount()
  })
})
