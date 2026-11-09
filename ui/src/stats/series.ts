// Client-side history for sparklines. The server keeps none: each snapshot adds one point per series, so a freshly
// opened page starts empty. Unknown points are NaN inside the buffers and `undefined` to readers, never 0.

export const DEFAULT_CAPACITY = 120

/** A fixed-capacity ring of numbers; the oldest value is overwritten first. */
export class Ring {
  readonly capacity: number
  private readonly values: Float64Array
  private next = 0
  private size = 0

  constructor(capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new RangeError(`capacity must be a positive integer: ${capacity}`)
    this.capacity = capacity
    this.values = new Float64Array(capacity)
  }

  get length(): number { return this.size }

  push(value: number): void {
    this.values[this.next] = value
    this.next = (this.next + 1) % this.capacity
    this.size = Math.min(this.size + 1, this.capacity)
  }

  last(): number | undefined { return this.size === 0 ? undefined : this.values[(this.next + this.capacity - 1) % this.capacity] }

  /** Oldest first. */
  toArray(): number[] {
    const start = (this.next + this.capacity - this.size) % this.capacity
    return Array.from({ length: this.size }, (_, index) => this.values[(start + index) % this.capacity]!)
  }

  clear(): void {
    this.next = 0
    this.size = 0
  }
}

/** Series by key over one shared timeline of sample times. */
export class SeriesBook {
  private readonly capacity: number
  private readonly timeline: Ring
  private readonly rings = new Map<string, Ring>()
  private readonly absent = new Map<string, number>()

  constructor(capacity = DEFAULT_CAPACITY) {
    this.capacity = capacity
    this.timeline = new Ring(capacity)
  }

  /** Adds one sample. Keys missing from `values` get an unknown point; a key missing for a whole window is dropped. */
  record(atMs: number, values: ReadonlyMap<string, number>): void {
    for (const key of values.keys()) if (!this.rings.has(key)) this.rings.set(key, this.backfilled())
    for (const [key, ring] of this.rings) {
      const value = values.get(key)
      ring.push(value ?? Number.NaN)
      const missing = value === undefined ? (this.absent.get(key) ?? 0) + 1 : 0
      if (missing >= this.capacity) this.drop(key)
      else this.absent.set(key, missing)
    }
    this.timeline.push(atMs)
  }

  times(): number[] { return this.timeline.toArray() }

  /** Aligned with `times()`; empty for an unknown key. */
  series(key: string): Array<number | undefined> {
    return (this.rings.get(key)?.toArray() ?? []).map((value) => (Number.isNaN(value) ? undefined : value))
  }

  keys(): IterableIterator<string> { return this.rings.keys() }

  clear(): void {
    this.timeline.clear()
    this.rings.clear()
    this.absent.clear()
  }

  private backfilled(): Ring {
    const ring = new Ring(this.capacity)
    for (let index = 0; index < this.timeline.length; index++) ring.push(Number.NaN)
    return ring
  }

  private drop(key: string): void {
    this.rings.delete(key)
    this.absent.delete(key)
  }
}
