export type DiffKind = 'same' | 'added' | 'removed'

export interface DiffLine {
  kind: DiffKind
  text: string
  oldLine: number | null
  newLine: number | null
}

export interface DiffHunk {
  lines: DiffLine[]
}

const MAX_EDIT_DISTANCE = 2000

export function splitLines(text: string): string[] {
  if (text === '') return []
  const lines = text.split('\n')
  if (lines.at(-1) === '') lines.pop()
  return lines
}

function commonPrefix(a: string[], b: string[]): number {
  let length = 0
  while (length < a.length && length < b.length && a[length] === b[length]) length++
  return length
}

function commonSuffix(a: string[], b: string[], prefix: number): number {
  let length = 0
  while (length < a.length - prefix && length < b.length - prefix && a.at(-1 - length) === b.at(-1 - length)) length++
  return length
}

function shortestEditTrace(a: string[], b: string[]): Int32Array[] | null {
  const max = Math.min(a.length + b.length, MAX_EDIT_DISTANCE)
  const offset = max + 1
  const frontier = new Int32Array(2 * max + 3)
  const trace: Int32Array[] = []
  for (let distance = 0; distance <= max; distance++) {
    trace.push(frontier.slice())
    for (let diagonal = -distance; diagonal <= distance; diagonal += 2) {
      const down = diagonal === -distance ||
        (diagonal !== distance && frontier[offset + diagonal - 1]! < frontier[offset + diagonal + 1]!)
      let x = down ? frontier[offset + diagonal + 1]! : frontier[offset + diagonal - 1]! + 1
      let y = x - diagonal
      while (x < a.length && y < b.length && a[x] === b[y]) { x++; y++ }
      frontier[offset + diagonal] = x
      if (x >= a.length && y >= b.length) return trace
    }
  }
  return null
}

type Step = { kind: DiffKind; text: string }

function backtrack(a: string[], b: string[], trace: Int32Array[]): Step[] {
  const offset = (trace[0]!.length - 3) / 2 + 1
  const steps: Step[] = []
  let x = a.length
  let y = b.length
  for (let distance = trace.length - 1; distance >= 0; distance--) {
    const frontier = trace[distance]!
    const diagonal = x - y
    const down = diagonal === -distance ||
      (diagonal !== distance && frontier[offset + diagonal - 1]! < frontier[offset + diagonal + 1]!)
    const previousDiagonal = down ? diagonal + 1 : diagonal - 1
    const previousX = frontier[offset + previousDiagonal]!
    const previousY = previousX - previousDiagonal
    while (x > previousX && y > previousY) {
      x--
      y--
      steps.push({ kind: 'same', text: a[x]! })
    }
    if (distance === 0) break
    if (down) steps.push({ kind: 'added', text: b[previousY]! })
    else steps.push({ kind: 'removed', text: a[previousX]! })
    x = previousX
    y = previousY
  }
  return steps.reverse()
}

function replacement(a: string[], b: string[]): Step[] {
  return [
    ...a.map((text): Step => ({ kind: 'removed', text })),
    ...b.map((text): Step => ({ kind: 'added', text })),
  ]
}

function middleSteps(a: string[], b: string[]): Step[] {
  if (a.length === 0 || b.length === 0) return replacement(a, b)
  const trace = shortestEditTrace(a, b)
  return trace === null ? replacement(a, b) : backtrack(a, b, trace)
}

export function diffLines(before: string, after: string): DiffLine[] {
  const a = splitLines(before)
  const b = splitLines(after)
  const prefix = commonPrefix(a, b)
  const suffix = commonSuffix(a, b, prefix)
  const steps: Step[] = [
    ...a.slice(0, prefix).map((text): Step => ({ kind: 'same', text })),
    ...middleSteps(a.slice(prefix, a.length - suffix), b.slice(prefix, b.length - suffix)),
    ...a.slice(a.length - suffix).map((text): Step => ({ kind: 'same', text })),
  ]
  let oldLine = 0
  let newLine = 0
  return steps.map(({ kind, text }) => ({
    kind,
    text,
    oldLine: kind === 'added' ? null : ++oldLine,
    newLine: kind === 'removed' ? null : ++newLine,
  }))
}

export function diffHunks(lines: DiffLine[], context = 3): DiffHunk[] {
  const keep = new Array<boolean>(lines.length).fill(false)
  lines.forEach((line, index) => {
    if (line.kind === 'same') return
    for (let near = Math.max(0, index - context); near <= Math.min(lines.length - 1, index + context); near++) keep[near] = true
  })
  const hunks: DiffHunk[] = []
  lines.forEach((line, index) => {
    if (!keep[index]) return
    if (index === 0 || !keep[index - 1]) hunks.push({ lines: [] })
    hunks.at(-1)!.lines.push(line)
  })
  return hunks
}

export const changedLineCount = (lines: DiffLine[]): number => lines.filter(({ kind }) => kind !== 'same').length
