const STEPS: Record<string, number> = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }

export function nextFocusIndex(key: string, current: number, count: number): number | null {
  if (current < 0 || count === 0) return null
  if (key === 'Home') return 0
  if (key === 'End') return count - 1
  const step = STEPS[key]
  return step === undefined ? null : (current + step + count) % count
}
