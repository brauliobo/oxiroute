export type Json = unknown
export type JsonObject = Record<string, Json>
export type NamedItem = JsonObject & { name: string }

export interface Change {
  path: string
  before: Json
  after: Json
}

export const isObject = (value: Json): value is JsonObject =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

export function isNamedList(value: Json): value is NamedItem[] {
  if (!Array.isArray(value) || value.length === 0) return false
  const names = value.map((item) => (isObject(item) ? item.name : undefined))
  return names.every((name) => typeof name === 'string') && new Set(names).size === names.length
}

export function equal(left: Json, right: Json): boolean {
  if (left === right) return true
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((item, index) => equal(item, right[index]))
  }
  if (isObject(left) && isObject(right)) {
    const keys = Object.keys(left)
    return keys.length === Object.keys(right).length && keys.every((key) => key in right && equal(left[key], right[key]))
  }
  return false
}

export const pointer = (segments: readonly string[]): string =>
  `/${segments.map((segment) => segment.replaceAll('~', '~0').replaceAll('/', '~1')).join('/')}`

export function unionKeys(...objects: JsonObject[]): string[] {
  return [...new Set(objects.flatMap((object) => Object.keys(object)))]
}

export function unionNames(...lists: NamedItem[][]): string[] {
  return [...new Set(lists.flatMap((list) => list.map(({ name }) => name)))]
}

export const itemNamed = (list: NamedItem[], name: string): NamedItem | undefined =>
  list.find((item) => item.name === name)

export function changesBetween(before: Json, after: Json, path: readonly string[] = []): Change[] {
  if (equal(before, after)) return []
  if (isObject(before) && isObject(after)) {
    return unionKeys(before, after).flatMap((key) => changesBetween(before[key], after[key], [...path, key]))
  }
  if (isNamedList(before) && isNamedList(after)) {
    return unionNames(before, after).flatMap((name) =>
      changesBetween(itemNamed(before, name), itemNamed(after, name), [...path, name]))
  }
  return [{ path: pointer(path), before, after }]
}
