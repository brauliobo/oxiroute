import { describe, expect, it } from 'vitest'

import { changedLineCount, diffHunks, diffLines, splitLines, type DiffLine } from './lines'

const text = (lines: string[]): string => lines.map((line) => `${line}\n`).join('')
const side = (diff: DiffLine[], kinds: string[]): string[] => diff.filter(({ kind }) => kinds.includes(kind)).map(({ text }) => text)

function lcsLength(a: string[], b: string[]): number {
  const table = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0))
  for (let i = 1; i <= a.length; i++) {
    for (let j = 1; j <= b.length; j++) {
      table[i]![j] = a[i - 1] === b[j - 1] ? table[i - 1]![j - 1]! + 1 : Math.max(table[i - 1]![j]!, table[i]![j - 1]!)
    }
  }
  return table[a.length]![b.length]!
}

function seeded(seed: number): () => number {
  let state = seed
  return () => {
    state = (state * 1_664_525 + 1_013_904_223) % 4_294_967_296
    return state / 4_294_967_296
  }
}

describe('splitLines', () => {
  it('drops the trailing newline and keeps empty inner lines', () => {
    expect(splitLines('')).toEqual([])
    expect(splitLines('a\n')).toEqual(['a'])
    expect(splitLines('a\n\nb')).toEqual(['a', '', 'b'])
  })
})

describe('diffLines', () => {
  it('reports nothing for two empty texts', () => {
    expect(diffLines('', '')).toEqual([])
  })

  it('marks every line added or removed against an empty side', () => {
    expect(diffLines('', 'a\nb\n').map(({ kind }) => kind)).toEqual(['added', 'added'])
    expect(diffLines('a\nb\n', '').map(({ kind }) => kind)).toEqual(['removed', 'removed'])
  })

  it('reports identical texts as unchanged with matching line numbers', () => {
    const diff = diffLines('a\nb\nc\n', 'a\nb\nc\n')
    expect(changedLineCount(diff)).toBe(0)
    expect(diff.map(({ oldLine, newLine }) => [oldLine, newLine])).toEqual([[1, 1], [2, 2], [3, 3]])
  })

  it('shows a changed line as a removal followed by an addition and numbers both sides', () => {
    expect(diffLines('a\nb\nc\n', 'a\nB\nc\n')).toEqual([
      { kind: 'same', text: 'a', oldLine: 1, newLine: 1 },
      { kind: 'removed', text: 'b', oldLine: 2, newLine: null },
      { kind: 'added', text: 'B', oldLine: null, newLine: 2 },
      { kind: 'same', text: 'c', oldLine: 3, newLine: 3 },
    ])
  })

  it('handles insertions and deletions at both ends', () => {
    const diff = diffLines('b\nc\n', 'a\nb\nc\nd\n')
    expect(diff.map(({ kind }) => kind)).toEqual(['added', 'same', 'same', 'added'])
  })

  it('reconstructs both inputs and is minimal for random inputs', () => {
    const random = seeded(7)
    for (let round = 0; round < 300; round++) {
      const alphabet = ['x', 'y', 'z', 'w']
      const make = () => Array.from({ length: Math.floor(random() * 14) }, () => alphabet[Math.floor(random() * alphabet.length)]!)
      const before = make()
      const after = make()
      const diff = diffLines(text(before), text(after))
      expect(side(diff, ['same', 'removed'])).toEqual(before)
      expect(side(diff, ['same', 'added'])).toEqual(after)
      expect(diff.filter(({ kind }) => kind === 'same')).toHaveLength(lcsLength(before, after))
    }
  })

  it('diffs large inputs with a few edits quickly and minimally', () => {
    const before = Array.from({ length: 50_000 }, (_, index) => `line ${index}`)
    const after = before.filter((_, index) => index % 5_000 !== 0).concat('tail')
    after.splice(20_000, 0, 'inserted')
    const diff = diffLines(text(before), text(after))
    expect(side(diff, ['same', 'removed'])).toEqual(before)
    expect(side(diff, ['same', 'added'])).toEqual(after)
    expect(changedLineCount(diff)).toBe(10 + 2)
  })

  it('stays correct when inputs share nothing and exceed the edit-distance bound', () => {
    const before = Array.from({ length: 4_000 }, (_, index) => `old ${index}`)
    const after = Array.from({ length: 4_000 }, (_, index) => `new ${index}`)
    const diff = diffLines(text(before), text(after))
    expect(side(diff, ['same', 'removed'])).toEqual(before)
    expect(side(diff, ['same', 'added'])).toEqual(after)
    expect(changedLineCount(diff)).toBe(8_000)
  })
})

describe('diffHunks', () => {
  it('returns no hunks for identical input', () => {
    expect(diffHunks(diffLines('a\nb\n', 'a\nb\n'))).toEqual([])
  })

  it('keeps context lines around changes and splits distant changes', () => {
    const before = Array.from({ length: 30 }, (_, index) => `l${index}`)
    const after = [...before]
    after[2] = 'changed-early'
    after[25] = 'changed-late'
    const hunks = diffHunks(diffLines(text(before), text(after)), 2)
    expect(hunks).toHaveLength(2)
    expect(hunks[0]!.lines.map(({ text: line }) => line)).toEqual(['l0', 'l1', 'l2', 'changed-early', 'l3', 'l4'])
    expect(hunks[1]!.lines.at(-1)!.text).toBe('l27')
  })
})
