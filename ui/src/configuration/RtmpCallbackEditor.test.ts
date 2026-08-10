import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import { defaultRtmpApplication, defaultRtmpCallback } from './canonicalDefaults'
import RtmpCallbackEditor from './RtmpCallbackEditor.vue'

describe('RTMP callback ownership', () => {
  it('edits the shared callback contract at a caller-supplied field path', async () => {
    const callbacks = defaultRtmpCallback()
    const wrapper = mount(RtmpCallbackEditor, {
      props: {
        callbacks,
        fieldPath: 'rtmp_services[].callbacks',
        legend: 'Service callbacks',
      },
    })

    await wrapper.get('[data-field="rtmp_services[].callbacks.on_connect"] input').setValue('https://callbacks.test/connect')
    await wrapper.get('[data-field="rtmp_services[].callbacks.notify_method"] select').setValue('get')
    await wrapper.get('[data-field="rtmp_services[].callbacks.notify_update_strict"] input').setValue(true)

    expect(callbacks).toMatchObject({
      on_connect: 'https://callbacks.test/connect',
      notify_method: 'get',
      notify_update_strict: true,
    })
  })

  it('creates independent canonical applications with disabled optional media policies', () => {
    const first = defaultRtmpApplication()
    const second = defaultRtmpApplication()

    first.callbacks.on_publish = 'https://callbacks.test/publish'
    expect(second.callbacks.on_publish).toBeNull()
    expect(first).toMatchObject({ hls: null, dash: null, vod: null, recorders: [] })
  })
})
