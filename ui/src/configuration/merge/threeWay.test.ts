import { describe, expect, it } from 'vitest'

import { canonicalConfig } from '../../test/configurationWorkspace'
import { mergeConfig } from './threeWay'
import { changesBetween, equal, isNamedList, isObject, itemNamed, type Json } from './tree'

function seeded(seed: number): () => number {
  let state = seed
  return () => {
    state = (state * 1_664_525 + 1_013_904_223) % 4_294_967_296
    return state / 4_294_967_296
  }
}

type Random = () => number
const pick = <T>(random: Random, items: readonly T[]): T => items[Math.floor(random() * items.length)]!

function randomNode(random: Random, depth: number): Json {
  const kind = depth <= 0 ? 'leaf' : pick(random, ['object', 'object', 'named', 'list', 'leaf'] as const)
  if (kind === 'object') {
    return Object.fromEntries(Array.from({ length: 1 + Math.floor(random() * 3) }, (_, index) =>
      [`k${index}`, randomNode(random, depth - 1)]))
  }
  if (kind === 'named') {
    return Array.from({ length: 1 + Math.floor(random() * 3) }, (_, index) =>
      ({ name: `n${index}`, value: randomNode(random, depth - 1) }))
  }
  if (kind === 'list') return Array.from({ length: Math.floor(random() * 3) }, () => pick(random, ['a', 'b', 'c']))
  return pick(random, [0, 1, 2, 'x', 'y', null, true])
}

function randomEdit(random: Random, node: Json, depth: number): Json {
  if (random() < 0.2) return randomNode(random, Math.min(depth, 1))
  if (isObject(node)) {
    const keys = Object.keys(node)
    const next = { ...node }
    const key = keys.length > 0 && random() < 0.8 ? pick(random, keys) : `k${keys.length}`
    if (key in next && random() < 0.15) delete next[key]
    else next[key] = key in next ? randomEdit(random, next[key], depth - 1) : randomNode(random, depth - 1)
    return next
  }
  if (isNamedList(node)) {
    const next = node.map((item) => ({ ...item }))
    const index = Math.floor(random() * next.length)
    const roll = random()
    if (roll < 0.2 && next.length > 1) next.splice(index, 1)
    else if (roll < 0.4) next.push({ name: `n${next.length + Math.floor(random() * 3)}`, value: randomNode(random, depth - 1) })
    else next[index] = randomEdit(random, next[index], depth - 1) as (typeof next)[number]
    return isNamedList(next) ? next : node
  }
  return randomNode(random, 0)
}

function valueAt(root: Json, pointerPath: string): Json {
  return pointerPath.split('/').slice(1).filter((segment) => segment !== '').reduce<Json>((node, raw) => {
    const segment = raw.replaceAll('~1', '/').replaceAll('~0', '~')
    if (isObject(node)) return node[segment]
    if (isNamedList(node)) return itemNamed(node, segment)
    return undefined
  }, root)
}

const covers = (collisionPath: string, path: string): boolean =>
  collisionPath === '/' || collisionPath === path || path.startsWith(`${collisionPath}/`)

describe('mergeConfig', () => {
  const base = canonicalConfig()
  const clone = () => structuredClone(base)

  it('returns the disk version when nothing was edited', () => {
    const theirs = clone()
    theirs.version = 2
    const result = mergeConfig(base, clone(), theirs)
    expect(result.merged).toEqual(theirs)
    expect(result.applied).toEqual([])
    expect(result.collisions).toEqual([])
  })

  it('returns the edits when the disk did not change', () => {
    const mine = clone()
    mine.listeners[0]!.max_connections = 5
    const result = mergeConfig(base, mine, clone())
    expect(result.merged).toEqual(mine)
    expect(result.applied).toEqual([{ path: '/listeners/https/max_connections', before: base.listeners[0]!.max_connections, after: 5 }])
    expect(result.collisions).toEqual([])
  })

  it('reapplies edits to other settings and named items onto the new disk version', () => {
    const mine = clone()
    mine.listeners[0]!.max_connections = 5
    mine.upstream_pools[0]!.servers[0]!.endpoint = { type: 'socket', address: '192.0.2.10:3000' }
    const theirs = clone()
    theirs.version = 2
    theirs.listeners[1]!.max_connections = 7
    theirs.upstream_pools[0]!.servers[1]!.max_connections = 3

    const result = mergeConfig(base, mine, theirs)

    expect(result.collisions).toEqual([])
    expect(result.merged.version).toBe(2)
    expect(result.merged.listeners[0]!.max_connections).toBe(5)
    expect(result.merged.listeners[1]!.max_connections).toBe(7)
    expect(result.merged.upstream_pools[0]!.servers[0]!.endpoint).toEqual({ type: 'socket', address: '192.0.2.10:3000' })
    expect(result.merged.upstream_pools[0]!.servers[1]!.max_connections).toBe(3)
    expect(result.applied.map(({ path }) => path)).toEqual([
      '/listeners/https/max_connections',
      '/upstream_pools/web-origins/servers/server-1/endpoint/address',
    ])
  })

  it('keeps the disk value for the same setting and reports the collision', () => {
    const mine = clone()
    mine.listeners[0]!.max_connections = 5
    mine.version = 3
    const theirs = clone()
    theirs.listeners[0]!.max_connections = 9

    const result = mergeConfig(base, mine, theirs)

    expect(result.merged.listeners[0]!.max_connections).toBe(9)
    expect(result.merged.version).toBe(3)
    expect(result.collisions).toEqual([
      { path: '/listeners/https/max_connections', base: base.listeners[0]!.max_connections, mine: 5, theirs: 9 },
    ])
    expect(result.applied.map(({ path }) => path)).toEqual(['/version'])
  })

  it('does not report identical edits as collisions', () => {
    const mine = clone()
    mine.version = 4
    const theirs = clone()
    theirs.version = 4
    const result = mergeConfig(base, mine, theirs)
    expect(result.merged.version).toBe(4)
    expect(result.collisions).toEqual([])
  })

  it('merges items added on both sides by name and collides when one side deleted what the other edited', () => {
    const mine = clone()
    mine.cache_stores = [...mine.cache_stores, { ...structuredClone(mine.cache_stores[0] ?? ({} as never)), name: 'mine-store' }]
    mine.listeners = mine.listeners.slice(1)
    const theirs = clone()
    theirs.listeners[0]!.max_connections = 8
    theirs.upstream_pools.push({ ...structuredClone(theirs.upstream_pools[0]!), name: 'theirs-pool' })

    const result = mergeConfig(base, mine, theirs)

    expect(result.merged.upstream_pools.map(({ name }) => name)).toContain('theirs-pool')
    expect(result.collisions.map(({ path }) => path)).toEqual(['/listeners/https'])
    expect(result.merged.listeners.map(({ name }) => name)).toContain('https')
  })

  it('treats lists without item names as one unit', () => {
    const mine = clone()
    mine.certificates[0]!.dns_names = ['mine.example.test']
    const theirs = clone()
    theirs.certificates[0]!.dns_names = ['theirs.example.test']
    const result = mergeConfig(base, mine, theirs)
    expect(result.collisions.map(({ path }) => path)).toEqual(['/certificates/direct-public/dns_names'])
    expect(result.merged.certificates[0]!.dns_names).toEqual(['theirs.example.test'])
  })
})

describe('mergeConfig properties', () => {
  const rounds = 400

  function samples() {
    const random = seeded(20_260_518)
    return Array.from({ length: rounds }, () => {
      const base = randomNode(random, 3)
      const edit = () => {
        let next = base
        for (let count = 0; count < 1 + Math.floor(random() * 3); count++) next = randomEdit(random, next, 3)
        return next
      }
      return { base, mine: edit(), theirs: edit() }
    })
  }

  it('merge(base, base, theirs) = theirs and merge(base, mine, base) = mine without collisions', () => {
    for (const { base, mine, theirs } of samples()) {
      const keepTheirs = mergeConfig(base, base, theirs)
      expect(keepTheirs.merged).toEqual(theirs)
      expect(keepTheirs.collisions).toEqual([])
      const keepMine = mergeConfig(base, mine, base)
      expect(keepMine.merged).toEqual(mine)
      expect(keepMine.collisions).toEqual([])
    }
  })

  it('is the identity when nothing changed and agrees when both sides made the same edit', () => {
    for (const { base, mine } of samples()) {
      expect(mergeConfig(base, base, base).merged).toEqual(base)
      const same = mergeConfig(base, mine, mine)
      expect(same.merged).toEqual(mine)
      expect(same.collisions).toEqual([])
    }
  })

  it('never loses a change silently: each edit of mine is applied or reported, each edit of theirs is kept', () => {
    for (const { base, mine, theirs } of samples()) {
      const { merged, collisions } = mergeConfig(base, mine, theirs)
      for (const change of changesBetween(base, mine)) {
        const preserved = equal(valueAt(merged, change.path), change.after)
        const reported = collisions.some(({ path }) => covers(path, change.path))
        expect(preserved || reported, `lost ${change.path}`).toBe(true)
      }
      for (const change of changesBetween(base, theirs)) {
        const kept = equal(valueAt(merged, change.path), change.after)
        const replacedByMine = changesBetween(base, mine).some(({ path }) => covers(path, change.path) || covers(change.path, path))
        expect(kept || replacedByMine, `dropped disk change ${change.path}`).toBe(true)
      }
    }
  })

  it('only reports a collision where both sides changed the same setting differently', () => {
    for (const { base, mine, theirs } of samples()) {
      for (const collision of mergeConfig(base, mine, theirs).collisions) {
        expect(equal(collision.mine, collision.base)).toBe(false)
        expect(equal(collision.theirs, collision.base)).toBe(false)
        expect(equal(collision.mine, collision.theirs)).toBe(false)
      }
    }
  })

  it('finds the same collisions whichever side is called mine', () => {
    for (const { base, mine, theirs } of samples()) {
      const forward = mergeConfig(base, mine, theirs).collisions.map(({ path }) => path).sort()
      const backward = mergeConfig(base, theirs, mine).collisions.map(({ path }) => path).sort()
      expect(forward).toEqual(backward)
    }
  })
})
