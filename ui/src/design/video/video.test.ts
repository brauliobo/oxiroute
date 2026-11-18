import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '../../test/mount'
import { HLS_MIME_TYPE, playsHls } from './support'
import Video from './Video.vue'

afterEach(() => vi.restoreAllMocks())

describe('Video', () => {
  it('is a labelled muted video with controls that starts by itself', () => {
    const wrapper = mount(Video, { props: { src: '/media/a.m3u8', label: 'Front door preview' } })
    const video = wrapper.get('video').element as HTMLVideoElement
    expect(video.getAttribute('src')).toBe('/media/a.m3u8')
    expect(video.getAttribute('aria-label')).toBe('Front door preview')
    expect(video.controls).toBe(true)
    expect(video.autoplay).toBe(true)
    expect(video.muted).toBe(true)
    expect(video.hasAttribute('playsinline')).toBe(true)
  })

  it('reports that playback started and that it failed', async () => {
    const wrapper = mount(Video, { props: { src: '/media/a.m3u8', label: 'Preview' } })
    await wrapper.get('video').trigger('playing')
    await wrapper.get('video').trigger('error')
    expect(wrapper.emitted('playing')).toHaveLength(1)
    expect(wrapper.emitted('failed')).toHaveLength(1)
  })
})

describe('playsHls', () => {
  it('asks the browser about the HLS media type', () => {
    const canPlayType = vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockReturnValue('maybe')
    expect(playsHls()).toBe(true)
    expect(canPlayType).toHaveBeenCalledWith(HLS_MIME_TYPE)
    canPlayType.mockReturnValue('')
    expect(playsHls()).toBe(false)
  })
})
