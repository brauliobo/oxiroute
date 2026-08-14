import { existsSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const INVENTORY_SCHEMA = 4
const IGNORED_ATTRIBUTES = new Set(['automatically_derived'])

export function canonicalInventory(rustdoc, metadata = {}) {
  const index = rustdoc.index
  const paths = rustdoc.paths
  const root = item(rustdoc.root)
  const crateName = metadata.crateName ?? 'oxiroute-rtmp'
  const aliases = publicAliases(rustdoc)
  const dependencyAliases = dependencyPublicAliases(metadata.dependencies ?? {})
  const exportsByKey = new Map()
  const pendingExports = new Map()

  collectExports(root, [], new Set())
  const exportNames = new Map(
    [...aliases].map(([id, names]) => [id, [...names].sort(aliasOrder)[0]]),
  )
  for (const [key, { name, id }] of pendingExports) exportsByKey.set(key, renderExport(name, id))
  const exports = [...exportsByKey.values()]
  exports.sort((left, right) => compare(left.name, right.name) || compare(left.kind, right.kind))

  return [
    `# ${crateName} canonical public API v${INVENTORY_SCHEMA}`,
    `toolchain=${metadata.toolchain ?? '1.97.1'}`,
    `target=${metadata.target ?? 'unknown'}`,
    `features=${metadata.features ?? 'all'}`,
    `schema=${INVENTORY_SCHEMA}`,
    `commit=${metadata.commit ?? 'synthetic'}`,
    `rustdoc_json_format=${rustdoc.format_version}`,
    `exports=${exports.length}`,
    ...exports.map(({ kind, name, api }) => `${kind} ${name} ${stableStringify(api)}`),
    '',
  ].join('\n')

  function collectExports(module, prefix, activeModules) {
    const moduleId = String(module.id)
    if (activeModules.has(moduleId)) return
    const active = new Set(activeModules)
    active.add(moduleId)

    for (const id of module.inner.module.items) {
      const entry = item(id)
      const use = entry.inner.use
      if (use) {
        if (entry.visibility !== 'public') continue
        if (use.id === null) continue
        const target = index[String(use.id)]
        if (use.is_glob) {
          if (target?.inner.module) collectExports(target, prefix, active)
          continue
        }
        const name = [...prefix, use.name].join('::')
        if (target?.inner.module) {
          addModule(name)
          collectExports(target, [...prefix, use.name], active)
        } else addExport(name, String(use.id))
        continue
      }
      if (entry.visibility !== 'public' || !entry.name) continue
      const name = [...prefix, entry.name].join('::')
      if (entry.inner.module) {
        addModule(name)
        collectExports(entry, [...prefix, entry.name], active)
      } else addExport(name, String(entry.id))
    }
  }

  function addModule(name) {
    exportsByKey.set(`module ${name}`, { kind: 'module', name, api: {} })
  }

  function addExport(name, id) {
    const target = index[id]
    const kind = (paths[id]?.crate_id !== undefined && paths[id].crate_id !== 0)
      || (target?.crate_id !== undefined && target.crate_id !== 0)
      ? 'reexport'
      : Object.keys(target.inner)[0]
    pendingExports.set(`${kind} ${name}`, { name, id })
  }

  function item(id) {
    const value = index[String(id)]
    if (!value) throw new Error(`rustdoc item ${id} is missing`)
    return value
  }

  function canonicalPath(id, fallback) {
    const key = String(id)
    const exported = exportNames.get(key)
    if (exported) return `crate::${exported}`
    const path = paths[key]
    if (path?.crate_id !== undefined && path.crate_id !== 0) {
      const dependency = rustdoc.external_crates?.[String(path.crate_id)]?.name
      if (dependencyAliases.has(dependency)) {
        const definition = path.path.join('::')
        const publicPath = dependencyAliases.get(dependency).get(definition)
        if (!publicPath) {
          throw new Error(`first-party dependency path is not root-reachable: ${definition}`)
        }
        return `${dependency}::${publicPath}`
      }
    }
    if (path) return path.path.join('::')
    return fallback
  }

  function normalize(value) {
    if (value === null || typeof value !== 'object') return value
    if (Array.isArray(value)) return value.map(normalize)
    if (value.resolved_path) {
      const resolved = value.resolved_path
      return {
        resolved_path: {
          path: canonicalPath(resolved.id, resolved.path),
          args: normalize(resolved.args),
        },
      }
    }
    if (typeof value.path === 'string' && Object.hasOwn(value, 'id') && Object.hasOwn(value, 'args')) {
      const result = {
        path: canonicalPath(value.id, value.path),
        args: normalize(value.args),
      }
      for (const key of Object.keys(value).sort(compare)) {
        if (key === 'id' || key === 'path' || key === 'args') continue
        result[key] = normalize(value[key])
      }
      return result
    }
    const result = {}
    for (const key of Object.keys(value).sort(compare)) {
      if (key === 'id' || key === 'has_body') continue
      result[key] = normalize(value[key])
    }
    return result
  }

  function semanticAttrs(attrs) {
    return attrs
      .filter((attr) => {
        if (typeof attr === 'string') return !IGNORED_ATTRIBUTES.has(attr) && !attr.startsWith('doc')
        return !Object.hasOwn(attr, 'must_use') && !Object.hasOwn(attr, 'doc')
      })
      .map(normalize)
  }

  function renderExport(name, id) {
    const path = paths[id]
    const target = index[id]
    if ((path?.crate_id !== undefined && path.crate_id !== 0)
      || (target?.crate_id !== undefined && target.crate_id !== 0)) {
      return {
        kind: 'reexport',
        name,
        api: { target: canonicalPath(id, target?.name ?? name) },
      }
    }
    if (!target) throw new Error(`rustdoc item ${id} is missing`)
    const [kind, inner] = Object.entries(target.inner)[0]
    return { kind, name, api: renderItem(target, kind, inner) }
  }

  function renderItem(target, kind, inner) {
    const common = {}
    const attrs = semanticAttrs(target.attrs)
    if (attrs.length > 0) common.attrs = attrs
    if (target.deprecation) common.deprecation = normalize(target.deprecation)

    switch (kind) {
      case 'function':
        return { ...common, ...renderFunction(inner) }
      case 'constant':
      case 'static':
        return { ...common, ...normalize(inner) }
      case 'struct':
        return { ...common, ...renderStruct(inner) }
      case 'union':
        return {
          ...common,
          generics: normalize(inner.generics),
          fields: inner.fields.map(renderField).filter(Boolean).sort(named),
          impls: renderImpls(inner.impls),
        }
      case 'enum':
        return {
          ...common,
          generics: normalize(inner.generics),
          non_exhaustive: Boolean(inner.has_stripped_variants),
          variants: inner.variants.map(renderVariant).sort(named),
          impls: renderImpls(inner.impls),
        }
      case 'trait':
        return {
          ...common,
          is_auto: inner.is_auto,
          is_unsafe: inner.is_unsafe,
          is_dyn_compatible: inner.is_dyn_compatible,
          bounds: normalize(inner.bounds),
          generics: normalize(inner.generics),
          items: inner.items.map((id) => renderAssociated(id, true, true)).sort(named),
          implementations: renderImpls(inner.implementations),
        }
      case 'type_alias':
        return { ...common, ...normalize(inner) }
      default:
        return { ...common, definition: normalize(inner) }
    }
  }

  function renderFunction(value) {
    return {
      header: normalize(value.header),
      generics: normalize(value.generics),
      sig: normalize(value.sig),
    }
  }

  function renderStruct(value) {
    const [shape, shapeValue] = Object.entries(value.kind)[0]
    const result = {
      shape,
      generics: normalize(value.generics),
      impls: renderImpls(value.impls),
    }
    if (shape === 'plain') {
      result.has_private_fields = Boolean(shapeValue.has_stripped_fields)
      result.fields = shapeValue.fields.map(renderField).filter(Boolean).sort(named)
    } else if (shape === 'tuple') {
      result.fields = shapeValue
        .map((id) => id === null ? { private: true } : renderField(id, true))
    }
    return result
  }

  function renderField(id, includeDefault = false) {
    const field = item(id)
    if (!includeDefault && field.visibility !== 'public') return null
    return {
      name: field.name,
      type: normalize(field.inner.struct_field),
    }
  }

  function renderVariant(id) {
    const variant = item(id)
    const [shape, value] = Object.entries(variant.inner.variant.kind)[0]
    const result = { name: variant.name, shape }
    if (shape === 'tuple') result.fields = value.map((field) => renderField(field, true))
    if (shape === 'struct') result.fields = value.fields.map((field) => renderField(field, true)).sort(named)
    if (variant.inner.variant.discriminant) result.discriminant = normalize(variant.inner.variant.discriminant)
    return result
  }

  function renderImpls(ids) {
    return ids
      .map((id) => item(id).inner.impl)
      .filter((value) => value.blanket_impl === null)
      .map((value) => {
        if (value.trait === null) {
          const items = value.items
            .filter((id) => item(id).visibility === 'public')
            .map((id) => renderAssociated(id, false, false))
            .sort(named)
          return items.length === 0 ? null : {
            kind: 'inherent',
            generics: normalize(value.generics),
            items,
          }
        }
        return {
          kind: 'trait',
          target: normalize(value.for),
          trait: {
            path: canonicalPath(value.trait.id, value.trait.path),
            args: normalize(value.trait.args),
          },
          generics: normalize(value.generics),
          is_negative: value.is_negative,
          is_synthetic: value.is_synthetic,
          is_unsafe: value.is_unsafe,
          associated: value.items
            .filter((id) => index[String(id)] && !index[String(id)].inner.function)
            .map((id) => renderAssociated(id, true, false))
            .sort(named),
        }
      })
      .filter(Boolean)
      .sort((left, right) => compare(stableStringify(left), stableStringify(right)))
  }

  function renderAssociated(id, traitItem, includeRequired) {
    const entry = item(id)
    const [kind, inner] = Object.entries(entry.inner)[0]
    if (!traitItem && entry.visibility !== 'public') return null
    const result = { kind, name: entry.name }
    if (kind === 'function') {
      Object.assign(result, renderFunction(inner))
      if (includeRequired) result.required = !inner.has_body
    }
    else Object.assign(result, normalize(inner))
    return result
  }
}

function publicAliases(rustdoc) {
  const aliases = new Map()
  const index = rustdoc.index

  visit(index[String(rustdoc.root)], [], new Set())
  return aliases

  function visit(module, prefix, activeModules) {
    if (!module?.inner.module) return
    const moduleId = String(module.id)
    if (activeModules.has(moduleId)) return
    const active = new Set(activeModules)
    active.add(moduleId)

    for (const id of module.inner.module.items) {
      const entry = index[String(id)]
      if (!entry) throw new Error(`rustdoc item ${id} is missing`)
      const use = entry.inner.use
      if (use) {
        if (entry.visibility !== 'public' || use.id === null) continue
        const target = index[String(use.id)]
        if (use.is_glob) {
          visit(target, prefix, active)
        } else if (target?.inner.module) {
          visit(target, [...prefix, use.name], active)
        } else {
          add(String(use.id), [...prefix, use.name].join('::'))
        }
        continue
      }
      if (entry.visibility !== 'public' || !entry.name) continue
      if (entry.inner.module) visit(entry, [...prefix, entry.name], active)
      else add(String(entry.id), [...prefix, entry.name].join('::'))
    }
  }

  function add(id, name) {
    if (!aliases.has(id)) aliases.set(id, new Set())
    aliases.get(id).add(name)
  }
}

function dependencyPublicAliases(dependencies) {
  return new Map(Object.entries(dependencies).map(([crateName, rustdoc]) => {
    const rustdocCrateName = rustdoc.index[String(rustdoc.root)]?.name
    if (rustdocCrateName !== crateName) {
      throw new Error(`dependency rustdoc name mismatch: expected ${crateName}, got ${rustdocCrateName ?? 'unknown'}`)
    }
    const aliasesByDefinition = new Map()
    for (const [id, aliases] of publicAliases(rustdoc)) {
      const path = rustdoc.paths[String(id)]
      if (!path || path.crate_id !== 0) continue
      const definition = path.path.join('::')
      const alias = [...aliases].sort(aliasOrder)[0]
      const existing = aliasesByDefinition.get(definition)
      if (!existing || aliasOrder(alias, existing) < 0) aliasesByDefinition.set(definition, alias)
    }
    return [crateName, aliasesByDefinition]
  }))
}

function aliasOrder(left, right) {
  const leftDepth = left.split('::').length
  const rightDepth = right.split('::').length
  return leftDepth - rightDepth || compare(left, right)
}

export function classifiedDelta(baseline, candidate, crateName) {
  const baselineSchema = inventoryMetadata(baseline).get('schema')
  const candidateSchema = inventoryMetadata(candidate).get('schema')
  if (!baselineSchema || baselineSchema !== candidateSchema) {
    throw new Error(`canonical inventory schema mismatch: baseline=${baselineSchema ?? 'missing'}, candidate=${candidateSchema ?? 'missing'}`)
  }
  const before = inventoryEntries(baseline)
  const after = inventoryEntries(candidate)
  const keys = [...new Set([...before.keys(), ...after.keys()])].sort(compare)
  const removed = []
  const added = []
  const changed = []

  for (const key of keys) {
    const oldLine = before.get(key)
    const newLine = after.get(key)
    if (oldLine === undefined) added.push(newLine)
    else if (newLine === undefined) removed.push(oldLine)
    else if (oldLine !== newLine) changed.push([oldLine, newLine])
  }

  return [
    `## ${crateName}`,
    `removed=${removed.length}`,
    `added=${added.length}`,
    `changed=${changed.length}`,
    ...removed.map((line) => `- ${line}`),
    ...added.map((line) => `+ ${line}`),
    ...changed.flatMap(([oldLine, newLine]) => [`< ${oldLine}`, `> ${newLine}`]),
    '',
  ].join('\n')
}

function inventoryMetadata(inventory) {
  return new Map(inventory.split('\n').flatMap((line) => {
    const separator = line.indexOf('=')
    return separator > 0 ? [[line.slice(0, separator), line.slice(separator + 1)]] : []
  }))
}

function inventoryEntries(inventory) {
  const entries = new Map()
  for (const line of inventory.split('\n')) {
    if (!/^(?:constant|enum|function|module|reexport|static|struct|trait|type_alias|union) /.test(line)) continue
    if (!line) continue
    const separator = line.indexOf(' ', line.indexOf(' ') + 1)
    if (separator < 0) throw new Error(`invalid inventory entry: ${line}`)
    entries.set(line.slice(0, separator), line)
  }
  return entries
}

function stableStringify(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(',')}]`
  return `{${Object.keys(value).sort(compare).map((key) => `${JSON.stringify(key)}:${stableStringify(value[key])}`).join(',')}}`
}

function compare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0
}

function named(left, right) {
  return compare(left?.name ?? '', right?.name ?? '') || compare(stableStringify(left), stableStringify(right))
}

function selfTest() {
  const base = {
    root: 1,
    crate_version: '0.4.1',
    format_version: 57,
    includes_private: false,
    target: { triple: 'test-host', target_features: [] },
    paths: {
      2: { crate_id: 0, path: ['fixture', 'Public'], kind: 'struct' },
      20: { crate_id: 1, path: ['core', 'fmt', 'Debug'], kind: 'trait' },
      30: { crate_id: 2, path: ['dependency', 'External'], kind: 'struct' },
      40: { crate_id: 0, path: ['fixture', 'PublicTrait'], kind: 'trait' },
    },
    index: {
      1: publicItem('fixture', { module: { is_crate: true, is_stripped: false, items: [3, 11, 40] } }),
      2: publicItem('Public', {
        struct: {
          kind: { plain: { fields: [4, 5], has_stripped_fields: true } },
          generics: { params: [], where_predicates: [] },
          impls: [6, 8, 10],
        },
      }),
      3: publicItem(null, { use: { source: 'private::Public', name: 'Public', id: 2, is_glob: false } }),
      4: publicItem('value', { struct_field: { primitive: 'u64' } }),
      5: defaultItem('private_value', { struct_field: { primitive: 'u32' } }),
      6: defaultItem(null, { impl: implValue(null, [7, 9]) }),
      7: publicItem('value', functionInner()),
      8: defaultItem(null, { impl: implValue({ path: 'Debug', id: 20, args: null }, []) }),
      9: defaultItem('private_helper', functionInner()),
      10: defaultItem(null, { impl: { ...implValue({ path: 'Any', id: 21, args: null }, []), blanket_impl: { generic: 'T' } } }),
      11: publicItem(null, { use: { source: 'dependency::External', name: 'External', id: 30, is_glob: false } }),
      20: { ...publicItem('Debug', { trait: {} }), crate_id: 1 },
      40: publicItem('PublicTrait', {
        trait: {
          is_auto: false,
          is_unsafe: false,
          is_dyn_compatible: true,
          items: [41],
          generics: { params: [], where_predicates: [] },
          bounds: [],
          implementations: [43],
        },
      }),
      41: defaultItem('required_value', functionInner(false)),
      43: defaultItem(null, { impl: implValue({ path: 'PublicTrait', id: 40, args: null }, [44]) }),
      44: defaultItem('Output', {
        assoc_type: {
          generics: { params: [], where_predicates: [] },
          bounds: [],
          type: { primitive: 'u64' },
        },
      }),
    },
  }
  for (const [id, entry] of Object.entries(base.index)) entry.id = Number(id)
  const noisy = structuredClone(base)
  noisy.index[2].docs = 'changed docs'
  noisy.index[2].span = { filename: '/home/private/project/src/lib.rs', begin: [1, 1], end: [2, 1] }
  noisy.index[3].inner.use.source = '/home/private/project/src/private.rs::Public'
  noisy.index[43].inner.impl.trait.path = '/local/alias/PublicTrait'
  noisy.index[43].inner.impl.for.resolved_path.path = '/local/alias/Public'
  noisy.index[9].inner.function.sig.output = { primitive: 'u128' }
  noisy.index[9].inner.function.has_body = false
  noisy.index[20].docs = 'dependency docs changed'
  noisy.index[20].inner.trait = { dependency_body: 'changed' }
  noisy.index[99] = defaultItem('other_private_item', { function: functionInner().function })
  noisy.target = { triple: 'other-host', target_features: ['environment-specific'] }
  process.env.CARGO_HOME = '/private/cargo-home'

  const expected = canonicalInventory(base)
  const actual = canonicalInventory(noisy)
  if (expected !== actual) throw new Error('non-API rustdoc noise changed the canonical inventory')

  const changed = structuredClone(base)
  changed.index[4].inner.struct_field = { primitive: 'u128' }
  if (canonicalInventory(changed) === expected) throw new Error('public signature change was not detected')

  const provided = structuredClone(base)
  provided.index[41].inner.function.has_body = true
  const providedInventory = canonicalInventory(provided)
  if (providedInventory === expected) throw new Error('required trait method becoming provided was not detected')
  provided.index[41].inner.function.has_body = false
  if (canonicalInventory(provided) === providedInventory) throw new Error('provided trait method becoming required was not detected')

  const removedImpl = structuredClone(base)
  removedImpl.index[40].inner.trait.implementations = []
  const withoutImpl = canonicalInventory(removedImpl)
  if (withoutImpl === expected) throw new Error('removing a public trait implementation was not detected')
  removedImpl.index[40].inner.trait.implementations.push(43)
  if (canonicalInventory(removedImpl) === withoutImpl) throw new Error('adding a public trait implementation was not detected')

  const changedImplTarget = structuredClone(base)
  changedImplTarget.index[43].inner.impl.for = { primitive: 'u32' }
  if (canonicalInventory(changedImplTarget) === expected) throw new Error('public trait implementation target change was not detected')

  const changedImplBounds = structuredClone(base)
  changedImplBounds.index[43].inner.impl.generics.where_predicates.push({
    lifetime_predicate: { lifetime: "'a", outlives: ["'static"] },
  })
  if (canonicalInventory(changedImplBounds) === expected) throw new Error('trait implementation generic bound change was not detected')

  const changedAssociatedBinding = structuredClone(base)
  changedAssociatedBinding.index[44].inner.assoc_type.type = { primitive: 'u128' }
  if (canonicalInventory(changedAssociatedBinding) === expected) throw new Error('trait implementation associated binding change was not detected')

  const delta = classifiedDelta(expected, canonicalInventory(changed), 'fixture')
  if (!delta.includes('changed=1') || !delta.includes('< struct Public ') || !delta.includes('> struct Public ')) {
    throw new Error('classified delta did not retain the changed public signature')
  }
  const mismatchedSchema = expected.replace('schema=4', 'schema=3')
  try {
    classifiedDelta(mismatchedSchema, expected, 'fixture')
    throw new Error('mismatched canonical inventory schemas were compared')
  } catch (error) {
    if (!String(error).includes('schema mismatch')) throw error
  }

  reachabilitySelfTest()
  multiCrateSelfTest()
}

function reachabilitySelfTest() {
  const fixture = {
    root: 1,
    format_version: 57,
    target: { triple: 'test-target', target_features: [] },
    paths: {},
    index: {
      1: publicItem('fixture', moduleInner([2, 3, 4, 5, 6, 7])),
      2: defaultItem('private_impl', moduleInner([10, 11, 12, 13])),
      3: publicItem(null, useInner('private_impl', 'private_impl', 2, true)),
      4: publicItem(null, useInner('private_impl::Renamed', 'Alias', 11, false)),
      5: publicItem('nested', moduleInner([20, 21])),
      6: publicItem(null, useInner('private_impl', 'public_alias', 2, false)),
      7: defaultItem(null, useInner('private_impl::Renamed', 'PrivateAlias', 11, false)),
      10: publicItem('Globbed', emptyStruct([30])),
      11: publicItem('Renamed', emptyStruct([])),
      12: publicItem('deep', moduleInner([14, 15])),
      13: publicItem(null, useInner('fixture', 'fixture', 1, true)),
      14: publicItem('DeepItem', emptyStruct([])),
      15: publicItem(null, useInner('private_impl', 'private_impl', 2, true)),
      20: publicItem('NestedItem', emptyStruct([])),
      21: publicItem(null, useInner('private_impl::Renamed', 'NestedAlias', 11, false)),
      30: defaultItem(null, { impl: implValue(null, [31]) }),
      31: publicItem('new', functionInner()),
    },
  }
  for (const [id, entry] of Object.entries(fixture.index)) entry.id = Number(id)

  const inventory = canonicalInventory(fixture)
  const lines = inventoryEntries(inventory)
  for (const key of [
    'struct Globbed',
    'struct Renamed',
    'struct Alias',
    'module nested',
    'struct nested::NestedItem',
    'struct nested::NestedAlias',
    'module public_alias',
    'struct public_alias::Globbed',
    'module deep',
    'struct deep::DeepItem',
  ]) {
    if (!lines.has(key)) throw new Error(`reachable API alias is missing: ${key}; got ${[...lines.keys()].join(', ')}`)
  }
  for (const key of [
    'module private_impl',
    'struct private_impl::Globbed',
    'struct PrivateAlias',
    'module public_alias::deep::private_impl',
  ]) {
    if (lines.has(key)) throw new Error(`private or duplicate API path was invented: ${key}`)
  }
  if ([...lines].filter(([key]) => key === 'struct Alias').length !== 1) {
    throw new Error('duplicate aliases produced duplicate public records')
  }
  const globbed = lines.get('struct Globbed')
  if (!globbed.includes('"name":"new"')) throw new Error('reachable inherent implementation was omitted')

  const traitFixture = structuredClone(fixture)
  traitFixture.index[1].inner.module.items.push(40)
  traitFixture.index[40] = publicItem('PublicTrait', {
    trait: {
      is_auto: false,
      is_unsafe: false,
      is_dyn_compatible: true,
      items: [41],
      generics: { params: [], where_predicates: [] },
      bounds: [],
      implementations: [42],
    },
  })
  traitFixture.index[41] = defaultItem('required', functionInner(false))
  traitFixture.index[42] = defaultItem(null, {
    impl: implValue({ path: 'PublicTrait', id: 40, args: null }, []),
  })
  for (const [id, entry] of Object.entries(traitFixture.index)) entry.id = Number(id)
  const traitInventory = canonicalInventory(traitFixture)
  if (!traitInventory.includes('trait PublicTrait ') || !traitInventory.includes('"required":true')) {
    throw new Error('reachable trait requirements or implementations were omitted')
  }
}

function multiCrateSelfTest() {
  const dependency = {
    root: 1,
    format_version: 57,
    external_crates: {},
    paths: {
      2: { crate_id: 0, path: ['dependency', 'private', 'PublicType'], kind: 'struct' },
      3: { crate_id: 0, path: ['dependency', 'private', 'PublicTrait'], kind: 'trait' },
      4: { crate_id: 0, path: ['dependency', 'private', 'Associated'], kind: 'struct' },
    },
    index: {
      1: publicItem('dependency', moduleInner([10, 11, 12])),
      2: publicItem('PublicType', emptyStruct([])),
      3: publicItem('PublicTrait', { trait: { is_auto: false, is_unsafe: false, is_dyn_compatible: true, items: [], generics: { params: [], where_predicates: [] }, bounds: [], implementations: [] } }),
      4: publicItem('Associated', emptyStruct([])),
      10: publicItem(null, useInner('private::PublicType', 'Alias', 2, false)),
      11: publicItem(null, useInner('private::PublicTrait', 'TraitAlias', 3, false)),
      12: publicItem(null, useInner('private::Associated', 'AssociatedAlias', 4, false)),
    },
  }
  for (const [id, entry] of Object.entries(dependency.index)) entry.id = Number(id)

  const consumer = {
    root: 1,
    format_version: 57,
    external_crates: { 1: { name: 'dependency', html_root_url: null } },
    paths: {
      2: { crate_id: 0, path: ['consumer', 'Consumer'], kind: 'struct' },
      20: { crate_id: 1, path: ['dependency', 'private', 'PublicType'], kind: 'struct' },
      21: { crate_id: 1, path: ['dependency', 'private', 'PublicTrait'], kind: 'trait' },
      22: { crate_id: 1, path: ['dependency', 'private', 'Associated'], kind: 'struct' },
    },
    index: {
      1: publicItem('consumer', moduleInner([2])),
      2: publicItem('Consumer', {
        struct: {
          kind: { plain: { fields: [3], has_stripped_fields: false } },
          generics: { params: [], where_predicates: [] },
          impls: [4],
        },
      }),
      3: publicItem('value', {
        struct_field: {
          resolved_path: {
            path: 'dependency::private::PublicType',
            id: 20,
            args: { angle_bracketed: { args: [{ type: { resolved_path: { path: 'dependency::private::Associated', id: 22, args: null } } }], constraints: [] } },
          },
        },
      }),
      4: defaultItem(null, {
        impl: {
          ...implValue({ path: 'dependency::private::PublicTrait', id: 21, args: null }, [5]),
          for: { resolved_path: { path: 'dependency::private::PublicType', id: 20, args: null } },
        },
      }),
      5: defaultItem('Output', {
        assoc_type: {
          generics: { params: [], where_predicates: [] },
          bounds: [],
          type: { resolved_path: { path: 'dependency::private::Associated', id: 22, args: null } },
        },
      }),
      20: { ...publicItem('PublicType', emptyStruct([])), crate_id: 1 },
      21: { ...publicItem('PublicTrait', { trait: {} }), crate_id: 1 },
      22: { ...publicItem('Associated', emptyStruct([])), crate_id: 1 },
    },
  }
  for (const [id, entry] of Object.entries(consumer.index)) entry.id = Number(id)

  const inventory = canonicalInventory(consumer, { dependencies: { dependency } })
  for (const publicPath of ['dependency::Alias', 'dependency::TraitAlias', 'dependency::AssociatedAlias']) {
    if (!inventory.includes(publicPath)) throw new Error(`external public alias is missing: ${publicPath}`)
  }
  if (inventory.includes('dependency::private::')) {
    throw new Error('external private definition path leaked into canonical inventory')
  }

  const hiddenDependency = structuredClone(dependency)
  hiddenDependency.index[10].visibility = 'default'
  try {
    canonicalInventory(consumer, { dependencies: { dependency: hiddenDependency } })
    throw new Error('unreachable first-party dependency path was accepted')
  } catch (error) {
    if (!String(error).includes('not root-reachable')) throw error
  }
}

function publicItem(name, inner) {
  return { id: 0, crate_id: 0, name, span: null, visibility: 'public', docs: null, links: {}, attrs: [], deprecation: null, inner }
}

function defaultItem(name, inner) {
  return { ...publicItem(name, inner), visibility: 'default' }
}

function functionInner(hasBody = true) {
  return {
    function: {
      sig: { inputs: [], output: null, is_c_variadic: false },
      generics: { params: [], where_predicates: [] },
      header: { is_const: false, is_unsafe: false, is_async: false, abi: 'Rust' },
      has_body: hasBody,
    },
  }
}

function moduleInner(items) {
  return { module: { is_crate: false, is_stripped: false, items } }
}

function useInner(source, name, id, isGlob) {
  return { use: { source, name, id, is_glob: isGlob } }
}

function emptyStruct(impls) {
  return {
    struct: {
      kind: { plain: { fields: [], has_stripped_fields: false } },
      generics: { params: [], where_predicates: [] },
      impls,
    },
  }
}

function implValue(trait, items) {
  return {
    is_unsafe: false,
    generics: { params: [], where_predicates: [] },
    provided_trait_methods: [],
    trait,
    for: { resolved_path: { path: 'Public', id: 2, args: null } },
    items,
    is_negative: false,
    is_synthetic: false,
    blanket_impl: null,
  }
}

export function runCli(args = process.argv.slice(2)) {
  const [mode, jsonPath, snapshotPath, crateName = 'oxiroute-rtmp', deltaPath, commit = 'candidate', dependenciesPath] = args
  const cliMetadata = {
    crateName,
    toolchain: process.env.OXIROUTE_API_TOOLCHAIN ?? '1.97.1',
    target: process.env.OXIROUTE_API_TARGET,
    features: 'all',
    commit,
    dependencies: dependenciesPath ? loadDependencies(dependenciesPath) : {},
  }
  if (mode === '--self-test') {
    selfTest()
  } else if (['--check', '--print', '--write'].includes(mode) && jsonPath) {
    const rustdoc = JSON.parse(readFileSync(jsonPath))
    const inventory = canonicalInventory(rustdoc, cliMetadata)
    if (mode === '--print') process.stdout.write(inventory)
    else if (mode === '--write' && snapshotPath) writeFileSync(snapshotPath, inventory)
    else if (mode === '--check' && snapshotPath) {
      const expected = readFileSync(snapshotPath, 'utf8')
      if (inventory !== expected) {
        console.error(`RTMP public API differs from ${snapshotPath}`)
        process.exit(1)
      }
    } else {
      process.exitCode = 2
    }
  } else if (['--diff-check', '--diff-write'].includes(mode) && jsonPath && snapshotPath && deltaPath) {
    const baseline = readFileSync(jsonPath, 'utf8')
    const rustdoc = JSON.parse(readFileSync(snapshotPath))
    const candidate = canonicalInventory(rustdoc, cliMetadata)
    const delta = classifiedDelta(baseline, candidate, crateName)
    if (mode === '--diff-write') writeFileSync(deltaPath, delta)
    else if (delta !== readFileSync(deltaPath, 'utf8')) {
      console.error(`${crateName} public API delta differs from ${deltaPath}`)
      process.exit(1)
    }
  } else {
    console.error('usage: node <canonicalizer> --check|--print|--write <rustdoc.json> [snapshot] [crate] [unused] [commit] [dependencies.json]')
    console.error('       node <canonicalizer> --diff-check|--diff-write <baseline> <rustdoc.json> <crate> <delta> [commit] [dependencies.json]')
    console.error('       node <canonicalizer> --self-test')
    process.exitCode = 2
  }
}

function loadDependencies(path) {
  if (statSync(path).isDirectory()) {
    const names = {
      oxiroute_config: 'oxiroute_config',
      oxiroute_config_source: 'oxiroute_config_source',
      oxiroute_import: 'oxiroute_import',
      oxiroute_server: 'oxiroute_server',
      oxiroute_rtmp: 'oxiroute_rtmp',
    }
    return Object.fromEntries(Object.entries(names).flatMap(([crateName, jsonName]) => {
      const jsonPath = join(path, `${jsonName}.json`)
      return existsSync(jsonPath) ? [[crateName, JSON.parse(readFileSync(jsonPath))]] : []
    }))
  }
  return Object.fromEntries(
    Object.entries(JSON.parse(readFileSync(path, 'utf8')))
      .map(([name, jsonPath]) => [name, JSON.parse(readFileSync(jsonPath))]),
  )
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) runCli()
