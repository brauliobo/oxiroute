export const HLS_MIME_TYPE = 'application/vnd.apple.mpegurl'

export const playsHls = (): boolean => document.createElement('video').canPlayType(HLS_MIME_TYPE) !== ''
