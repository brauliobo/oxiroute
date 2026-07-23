export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

export function arrayOf<T>(value: unknown, predicate: (entry: unknown) => entry is T): value is T[]
export function arrayOf(value: unknown, predicate: (entry: unknown) => boolean): boolean
export function arrayOf(value: unknown, predicate: (entry: unknown) => boolean): boolean {
  return Array.isArray(value) && value.every(predicate)
}

export function isString(value: unknown): value is string {
  return typeof value === 'string'
}

export function nullableString(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

export function finiteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

export function safeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

export function nullableSafeInteger(value: unknown): value is number | null {
  return value === null || safeInteger(value)
}

export function integerInRange(value: unknown, minimum: number, maximum: number): value is number {
  return safeInteger(value) && value >= minimum && value <= maximum
}

export function decimalString(value: unknown): value is string {
  return typeof value === 'string' && /^(0|[1-9][0-9]*)$/.test(value)
}
