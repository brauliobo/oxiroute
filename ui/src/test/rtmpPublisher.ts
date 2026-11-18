import { createConnection } from 'node:net'
import type { Socket } from 'node:net'

const CHUNK_SIZE = 4096
const HANDSHAKE_SIZE = 1536
const COMMAND = 20
const DATA = 18
const AUDIO = 8
const VIDEO = 9
const SET_CHUNK_SIZE = 1
const WAIT_MS = 10_000

type Amf = string | number | boolean | null | { [key: string]: Amf } | { ecma: { [key: string]: Amf } }

export interface RtmpMessage {
  type: number
  streamId: number
  payload: Buffer
}

interface ChunkState {
  type: number
  length: number
  streamId: number
  extended: boolean
  parts: Buffer[]
  received: number
}

const HEADER_SIZES = [11, 7, 3, 0] as const

class MessageReader {
  private buffer: Buffer = Buffer.alloc(0)
  private chunkSize = 128
  private readonly states = new Map<number, ChunkState>()

  push(data: Buffer): RtmpMessage[] {
    this.buffer = Buffer.concat([this.buffer, data])
    const messages: RtmpMessage[] = []
    for (let step = this.next(); step !== undefined; step = this.next()) if (step !== null) messages.push(step)
    return messages
  }

  private next(): RtmpMessage | null | undefined {
    const bytes = this.buffer
    if (bytes.length < 1) return undefined
    const format = bytes[0]! >> 6
    let id = bytes[0]! & 0x3f
    let offset = 1
    if (id === 0 || id === 1) {
      offset = id + 2
      if (bytes.length < offset) return undefined
      id = id === 0 ? 64 + bytes[1]! : 64 + bytes[1]! + bytes[2]! * 256
    }
    if (bytes.length < offset + HEADER_SIZES[format]!) return undefined
    const state = this.states.get(id) ?? { type: 0, length: 0, streamId: 0, extended: false, parts: [], received: 0 }
    const timestamp = format <= 2 ? bytes.readUIntBE(offset, 3) : 0
    if (format <= 1) {
      state.length = bytes.readUIntBE(offset + 3, 3)
      state.type = bytes[offset + 6]!
    }
    if (format === 0) state.streamId = bytes.readUInt32LE(offset + 7)
    if (format <= 2) state.extended = timestamp === 0xffffff
    offset += HEADER_SIZES[format]! + (state.extended ? 4 : 0)
    const take = Math.min(this.chunkSize, state.length - state.received)
    if (bytes.length < offset + take) return undefined
    state.parts.push(bytes.subarray(offset, offset + take))
    state.received += take
    this.buffer = bytes.subarray(offset + take)
    this.states.set(id, state)
    if (state.received < state.length) return null
    const message = { type: state.type, streamId: state.streamId, payload: Buffer.concat(state.parts) }
    state.parts = []
    state.received = 0
    if (message.type === SET_CHUNK_SIZE) this.chunkSize = message.payload.readUInt32BE(0) & 0x7fffffff
    return message
  }
}

function amf(value: Amf): Buffer {
  if (value === null) return Buffer.from([0x05])
  if (typeof value === 'boolean') return Buffer.from([0x01, value ? 1 : 0])
  if (typeof value === 'number') {
    const number = Buffer.alloc(9)
    number.writeDoubleBE(value, 1)
    return number
  }
  if (typeof value === 'string') {
    const text = Buffer.from(value, 'utf8')
    return Buffer.concat([Buffer.from([0x02, text.length >> 8, text.length & 0xff]), text])
  }
  const ecma = 'ecma' in value
  const entries = Object.entries(ecma ? (value.ecma as { [key: string]: Amf }) : value)
  const head = ecma ? Buffer.concat([Buffer.from([0x08]), uint32(entries.length)]) : Buffer.from([0x03])
  const members = entries.map(([key, entry]) => Buffer.concat([Buffer.from([key.length >> 8, key.length & 0xff]), Buffer.from(key), amf(entry)]))
  return Buffer.concat([head, ...members, Buffer.from([0, 0, 0x09])])
}

function uint32(value: number): Buffer {
  const bytes = Buffer.alloc(4)
  bytes.writeUInt32BE(value)
  return bytes
}

function chunked(chunkStreamId: number, type: number, streamId: number, timestamp: number, payload: Buffer): Buffer {
  const header = Buffer.alloc(12)
  header[0] = chunkStreamId
  header.writeUIntBE(timestamp, 1, 3)
  header.writeUIntBE(payload.length, 4, 3)
  header[7] = type
  header.writeUInt32LE(streamId, 8)
  const pieces: Buffer[] = [header]
  for (let offset = 0; offset < payload.length; offset += CHUNK_SIZE) {
    if (offset > 0) pieces.push(Buffer.from([0xc0 | chunkStreamId]))
    pieces.push(payload.subarray(offset, offset + CHUNK_SIZE))
  }
  return Buffer.concat(pieces)
}

const text = (message: RtmpMessage) => message.payload.toString('latin1')

export interface AvcHeaders {
  sps: Buffer
  pps: Buffer
}

export class RtmpPublisher {
  private readonly reader = new MessageReader()
  private readonly inbox: RtmpMessage[] = []
  private waiting: (() => void) | null = null
  private streamId = 0

  private readonly socket: Socket

  private constructor(socket: Socket) {
    this.socket = socket
    socket.on('data', (data: Buffer) => {
      this.inbox.push(...this.reader.push(data))
      this.waiting?.()
    })
  }

  static async connect(socketPath: string, application: string): Promise<RtmpPublisher> {
    const socket = createConnection({ path: socketPath })
    await new Promise<void>((resolve, reject) => socket.once('connect', resolve).once('error', reject))
    const publisher = await RtmpPublisher.handshake(socket)
    publisher.send(chunked(2, SET_CHUNK_SIZE, 0, 0, uint32(CHUNK_SIZE)))
    publisher.command(0, 'connect', 1, {
      app: application, type: 'nonprivate', flashVer: 'oxiroute-ui-test', tcUrl: `rtmp://localhost/${application}`,
    })
    await publisher.expect((message) => message.type === COMMAND && text(message).includes('NetConnection.Connect.Success'))
    publisher.command(0, 'createStream', 2, null)
    const created = await publisher.expect((message) => message.type === COMMAND && text(message).startsWith('\x02\x00\x07_result'))
    publisher.streamId = created.payload.readDoubleBE(created.payload.length - 8)
    return publisher
  }

  private static async handshake(socket: Socket): Promise<RtmpPublisher> {
    const received = new Promise<Buffer>((resolve, reject) => {
      const parts: Buffer[] = []
      const onData = (data: Buffer) => {
        parts.push(data)
        const all = Buffer.concat(parts)
        if (all.length < 1 + 2 * HANDSHAKE_SIZE) return
        socket.off('data', onData)
        resolve(all)
      }
      socket.on('data', onData).once('error', reject)
    })
    socket.write(Buffer.concat([Buffer.from([3]), Buffer.alloc(HANDSHAKE_SIZE)]))
    const reply = await received
    socket.write(reply.subarray(1, 1 + HANDSHAKE_SIZE))
    const publisher = new RtmpPublisher(socket)
    const rest = reply.subarray(1 + 2 * HANDSHAKE_SIZE)
    if (rest.length > 0) publisher.inbox.push(...publisher.reader.push(rest))
    return publisher
  }

  async publish(streamName: string): Promise<string> {
    this.command(this.streamId, 'publish', 0, null, streamName, 'live')
    const answer = await this.expect((message) => message.type === COMMAND && text(message).includes('NetStream.Publish.'))
    return /NetStream\.Publish\.\w+/.exec(text(answer))![0]
  }

  metadata(properties: { [key: string]: Amf }): void {
    this.send(chunked(6, DATA, this.streamId, 0, Buffer.concat([amf('@setDataFrame'), amf('onMetaData'), amf({ ecma: properties })])))
  }

  avcHeaders({ sps, pps }: AvcHeaders, timestamp: number): void {
    const record = Buffer.concat([
      Buffer.from([1, sps[1]!, sps[2]!, sps[3]!, 0xff, 0xe1, sps.length >> 8, sps.length & 0xff]), sps,
      Buffer.from([1, pps.length >> 8, pps.length & 0xff]), pps,
    ])
    this.send(chunked(5, VIDEO, this.streamId, timestamp, Buffer.concat([Buffer.from([0x17, 0, 0, 0, 0]), record])))
  }

  keyframe(nal: Buffer, timestamp: number): void {
    const body = Buffer.concat([Buffer.from([0x17, 1, 0, 0, 0]), uint32(nal.length), nal])
    this.send(chunked(5, VIDEO, this.streamId, timestamp, body))
  }

  aacHeader(timestamp: number): void {
    this.send(chunked(4, AUDIO, this.streamId, timestamp, Buffer.from([0xaf, 0, 0x12, 0x10])))
  }

  aacFrame(timestamp: number): void {
    this.send(chunked(4, AUDIO, this.streamId, timestamp, Buffer.from([0xaf, 1, 0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c])))
  }

  async close(): Promise<void> {
    const closed = new Promise<void>((resolve) => this.socket.once('close', () => resolve()))
    this.socket.end()
    await closed
  }

  private command(streamId: number, name: string, transaction: number, ...rest: Amf[]): void {
    this.send(chunked(3, COMMAND, streamId, 0, Buffer.concat([amf(name), amf(transaction), ...rest.map(amf)])))
  }

  private send(bytes: Buffer): void {
    this.socket.write(bytes)
  }

  private expect(predicate: (message: RtmpMessage) => boolean): Promise<RtmpMessage> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = null
        reject(new Error('the RTMP server did not answer in time'))
      }, WAIT_MS)
      const check = () => {
        const index = this.inbox.findIndex(predicate)
        if (index < 0) return
        clearTimeout(timer)
        this.waiting = null
        resolve(this.inbox.splice(index, 1)[0]!)
      }
      this.waiting = check
      check()
    })
  }
}
