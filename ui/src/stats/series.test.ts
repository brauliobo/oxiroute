import { describe, expect, it } from 'vitest'
import { Ring, SeriesBook } from './series'

describe('Ring', () => {
  it('keeps the newest values, oldest first, once it wraps', () => {
    const ring = new Ring(3)
    ;[1, 2].forEach((value) => ring.push(value))
    expect(ring.toArray()).toEqual([1, 2])
    ;[3, 4, 5].forEach((value) => ring.push(value))
    expect(ring.toArray()).toEqual([3, 4, 5])
    expect(ring.length).toBe(3)
    expect(ring.last()).toBe(5)
  })

  it('has no last value when empty and rejects a capacity below one', () => {
    expect(new Ring(2).last()).toBeUndefined()
    expect(() => new Ring(0)).toThrow(RangeError)
  })

  it('stores NaN as unknown', () => {
    const ring = new Ring(2)
    ring.push(Number.NaN)
    expect(ring.toArray()).toHaveLength(1)
    expect(Number.isNaN(ring.last())).toBe(true)
  })

  it('empties on clear', () => {
    const ring = new Ring(2)
    ring.push(1)
    ring.clear()
    expect(ring.toArray()).toEqual([])
  })
})

describe('SeriesBook', () => {
  it('records every key against one shared timeline', () => {
    const book = new SeriesBook(4)
    book.record(1000, new Map([['a', 1], ['b', 10]]))
    book.record(2000, new Map([['a', 2], ['b', 20]]))
    expect(book.times()).toEqual([1000, 2000])
    expect(book.series('a')).toEqual([1, 2])
    expect(book.series('b')).toEqual([10, 20])
  })

  it('leaves unknown points undefined, for keys that appear late and keys that skip a sample', () => {
    const book = new SeriesBook(4)
    book.record(1000, new Map([['a', 1]]))
    book.record(2000, new Map([['a', 2], ['late', 5]]))
    book.record(3000, new Map([['late', 6]]))
    expect(book.series('late')).toEqual([undefined, 5, 6])
    expect(book.series('a')).toEqual([1, 2, undefined])
  })

  it('wraps at the capacity and keeps the series aligned with the times', () => {
    const book = new SeriesBook(3)
    for (let at = 1; at <= 5; at++) book.record(at * 1000, new Map([['a', at]]))
    expect(book.times()).toEqual([3000, 4000, 5000])
    expect(book.series('a')).toEqual([3, 4, 5])
  })

  it('has an empty series for a key it never saw', () => {
    expect(new SeriesBook(3).series('nope')).toEqual([])
  })

  it('drops a key that was absent for a whole window', () => {
    const book = new SeriesBook(2)
    book.record(1, new Map([['gone', 1], ['kept', 1]]))
    book.record(2, new Map([['kept', 2]]))
    expect([...book.keys()].sort()).toEqual(['gone', 'kept'])
    book.record(3, new Map([['kept', 3]]))
    expect([...book.keys()]).toEqual(['kept'])
  })

  it('empties on clear', () => {
    const book = new SeriesBook(2)
    book.record(1, new Map([['a', 1]]))
    book.clear()
    expect(book.times()).toEqual([])
    expect([...book.keys()]).toEqual([])
  })
})
