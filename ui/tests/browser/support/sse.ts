export type SseMockResponse = {
  kind: 'sse'
  body: string
}

export function sse(body: string): SseMockResponse {
  return { body, kind: 'sse' }
}

export function shutdownStream(): SseMockResponse {
  return sse('event: ready\ndata: {"cursor":0}\n\n' +
    'event: shutdown\ndata: {"reason":"server_shutdown"}\n\n')
}
