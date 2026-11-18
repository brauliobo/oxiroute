import {
  changesBetween,
  equal,
  isNamedList,
  isObject,
  itemNamed,
  pointer,
  unionKeys,
  unionNames,
  type Change,
  type Json,
  type JsonObject,
  type NamedItem,
} from './tree'

export interface Collision {
  path: string
  base: Json
  mine: Json
  theirs: Json
}

export interface MergeResult<T> {
  merged: T
  applied: Change[]
  collisions: Collision[]
}

interface Report {
  applied: Change[]
  collisions: Collision[]
}

function mergeObjects(base: JsonObject, mine: JsonObject, theirs: JsonObject, path: string[], report: Report): JsonObject {
  const merged: JsonObject = {}
  for (const key of unionKeys(theirs, mine, base)) {
    const value = mergeNode(base[key], mine[key], theirs[key], [...path, key], report)
    if (value !== undefined) merged[key] = value
  }
  return merged
}

function mergeNamedLists(base: NamedItem[], mine: NamedItem[], theirs: NamedItem[], path: string[], report: Report): NamedItem[] {
  const merged: NamedItem[] = []
  for (const name of unionNames(theirs, mine, base)) {
    const value = mergeNode(itemNamed(base, name), itemNamed(mine, name), itemNamed(theirs, name), [...path, name], report)
    if (value !== undefined) merged.push(value as NamedItem)
  }
  return merged
}

function mergeNode(base: Json, mine: Json, theirs: Json, path: string[], report: Report): Json {
  if (equal(mine, base)) return theirs
  if (equal(theirs, base)) {
    report.applied.push(...changesBetween(base, mine, path))
    return mine
  }
  if (equal(mine, theirs)) return mine
  if (isObject(base) && isObject(mine) && isObject(theirs)) return mergeObjects(base, mine, theirs, path, report)
  if (isNamedList(base) && isNamedList(mine) && isNamedList(theirs)) {
    return mergeNamedLists(base, mine, theirs, path, report)
  }
  report.collisions.push({ path: pointer(path), base, mine, theirs })
  return theirs
}

export function mergeConfig<T>(base: T, mine: T, theirs: T): MergeResult<T> {
  const report: Report = { applied: [], collisions: [] }
  const merged = mergeNode(base, mine, theirs, [], report) as T
  return { merged, ...report }
}
