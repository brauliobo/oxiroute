#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const sourceRoot = path.join(root, 'crates/oxiroute-server/src')
const compilerModules = new Set([
  'generation_compiler.rs',
  'planning_errors.rs',
  'rtmp_value_plan.rs',
  'runtime_policy.rs',
  'forward_proxy.rs',
  'health.rs',
  'http_action.rs',
  'listener_inventory.rs',
  'routing.rs',
  'rtmp_value_mapping.rs',
  'service_plan.rs',
  'tls/mod.rs',
  'tls/upstream.rs',
  'topology.rs',
])
const roots = ['generation_compiler.rs::GenerationCompiler::compile']
const acquisitionNames = /(^|::)(acquire|open|load|read|write|bind|connect|resolve|spawn|prepare_tls|to_socket_addrs)$/
const forbiddenImports = /^(std::(fs|net::ToSocketAddrs|thread)|openssl|pingora|oxiroute_cache::(Cache|DiskCache)|crate::(http_cache|secure_bearer))($|::|\{)/
const mixedModuleImports = new Map([
  ['health.rs', new Set([
    'pingora::{ ErrorType, http::RequestHeader, lb::{ Backend, health_check::{HealthCheck, HttpHealthCheck, TcpHealthCheck}, }, server::ShutdownWatch, services::{ServiceReadyNotifier, background::BackgroundService}, }',
  ])],
  ['http_action.rs', new Set([
    'openssl::{ hash::{Hasher, MessageDigest}, memcmp, sha::sha256, }',
  ])],
])
const reviewedPathCalls = new Map([
  ['Arc::', 'immutable reference-counted value construction'], ['Box::', 'owned value construction'],
  ['Bytes::', 'immutable byte value construction'], ['Duration::', 'duration value construction'],
  ['HashMap::', 'in-memory collection construction'], ['HashSet::', 'in-memory collection construction'],
  ['HeaderName::', 'HTTP header value parsing'], ['HeaderValue::', 'HTTP header value parsing'],
].map(([call, reason]) => [call, reason]))
const reviewedExternalAssociatedCalls = new Map([
  ['oxiroute_rtmp::RtmpAccessRulePlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpTokenPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpAccessPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpCredentialPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpClientPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpRelayPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpPushPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpPullPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpHlsPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpDashPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpMediaPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpVodPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpRecorderPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpExecEnvironmentPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpExecPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpApplicationPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpFanoutPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpAutoPushPlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpServicePlan::new', 'public external pure value constructor'],
  ['oxiroute_rtmp::RtmpCallbackPlan::new', 'public external pure value constructor'],
])
const reviewedExternalAssociatedTypes = new Set(
  [...reviewedExternalAssociatedCalls].map(([call]) => call.split('::').at(-2)),
)
for (const call of [
  'validate_health_check_config',
  'crate::PassiveFailurePolicy::validate',
  'HttpGzipPlan::compile',
  'PassiveFailurePolicy::from_config', 'ProxyPolicyPlan::compile', 'RedirectPlan::compile',
  'Route::new', 'RoutePolicyPlan::compile', 'RuntimeEndpoint::compile', 'StaticFilesBlueprint::compile',
  'RequestHeader::build',
  'StatusCode::', 'String::', 'Vec::', 'VodApplicationBlueprint::compile',
  'RtmpCallbackEndpointBlueprint::parse', 'RtmpAccessPolicy::new', 'RtmpAccessRule::new',
  'RtmpSessionCeilings::new', 'ExecLimits::new', 'ExecEnvironment::new', 'VodLimits::', 'RecordingStoreLimits::',
  'RecorderMediaMask::new', 'RecordingPathPolicy::new', 'RtmpNetwork::parse', 'CacheTimeline::new',
  'oxiroute_rtmp::RtmpAccessPolicy::new', 'oxiroute_rtmp::RtmpAccessRule::new',
  'oxiroute_rtmp::RtmpCallbackEndpointBlueprint::parse',
]) reviewedPathCalls.set(call, 'reviewed pure value compiler or constructor')
const languageCalls = new Set(['if', 'for', 'let', 'match', 'while', 'loop', 'Some', 'None', 'Ok', 'Err', 'Self'])
const reviewedValueMethods = new Map([
  'and_then', 'any', 'as_bytes', 'as_deref', 'as_deref_mut', 'as_ref', 'as_slice', 'chain', 'clone', 'cloned', 'collect', 'contains',
  'copied', 'count', 'enumerate', 'expect', 'filter', 'filter_map', 'find', 'first', 'flat_map',
  'flatten', 'fold', 'insert', 'into', 'into_boxed_slice', 'into_iter', 'is_empty', 'is_none', 'is_ok', 'is_some', 'is_zero', 'iter', 'join', 'len',
  'map', 'map_err', 'map_or', 'map_or_else', 'next', 'ok_or', 'ok_or_else', 'position', 'push', 'push_str',
  'sort_unstable', 'sum', 'then', 'then_some', 'to_owned', 'to_string', 'to_vec', 'transpose', 'unwrap_or', 'unwrap_or_default', 'unwrap_or_else', 'zip',
  'as_draft', 'service_id', 'applications', 'callbacks', 'outbound_policy', 'common', 'name',
  'rules', 'token', 'policy', 'action', 'network', 'fanout', 'max_subscribers',
  'max_queue_messages_per_subscriber', 'max_queue_bytes_per_subscriber', 'vod', 'limits', 'sources',
  'endpoint', 'method', 'timeout', 'update_timeout', 'update_strict', 'relay_redirect', 'path_value',
  'publish', 'play',
  'contextualize_profile', 'contextualize_application', 'contextualize_service', 'with_endpoint',
  'with_update_policy', 'with_outbound_policy', 'with_max_inbound_message_size',
  'with_window_ack_size', 'profile', 'config', 'transport', 'application', 'stream_name',
  'with_segment_policy', 'with_extensions',
  'port', 'is_absolute', 'to_str', 'from_pathname',
  'set_version', 'append_header',
  'parse',
].map((name) => [name, 'reviewed std/external immutable or local value operation']))

function mask(source) {
  const output = [...source]
  const blank = (start, end) => {
    for (let index = start; index < end; index += 1) if (output[index] !== '\n') output[index] = ' '
  }
  for (let index = 0; index < source.length;) {
    if (source.startsWith('//', index)) {
      const end = source.indexOf('\n', index)
      blank(index, end < 0 ? source.length : end)
      index = end < 0 ? source.length : end
      continue
    }
    if (source.startsWith('/*', index)) {
      let depth = 1
      let end = index + 2
      while (end < source.length && depth > 0) {
        if (source.startsWith('/*', end)) { depth += 1; end += 2 }
        else if (source.startsWith('*/', end)) { depth -= 1; end += 2 }
        else end += 1
      }
      blank(index, end)
      index = end
      continue
    }
    const raw = source.slice(index).match(/^(?:br|rb|r|b)?(#+)?"/)
    if (raw) {
      const hashes = raw[1] ?? ''
      const close = `"${hashes}`
      let end = index + raw[0].length
      while (end < source.length) {
        if (!hashes && source[end] === '\\') { end += 2; continue }
        if (source.startsWith(close, end)) { end += close.length; break }
        end += 1
      }
      blank(index, end)
      index = end
      continue
    }
    if (source[index] === "'" && /^'(?:\\.|[^'\\])'/.test(source.slice(index))) {
      const match = source.slice(index).match(/^'(?:\\.|[^'\\])'/)[0]
      blank(index, index + match.length)
      index += match.length
      continue
    }
    index += 1
  }
  return output.join('')
}

function matchingBrace(source, open) {
  let depth = 0
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === '{') depth += 1
    if (source[index] === '}' && --depth === 0) return index
  }
  throw new Error(`unclosed block at byte ${open}`)
}

function ownerAt(source, index) {
  const prefix = source.slice(0, index)
  const candidates = [...prefix.matchAll(/\bimpl(?:\s*<[^{}]*>)?\s+(?:(?:[^{}]+)\s+for\s+)?([A-Za-z_][A-Za-z0-9_:<>]*)\s*\{/g)]
  for (let candidate = candidates.length - 1; candidate >= 0; candidate -= 1) {
    const match = candidates[candidate]
    const open = match.index + match[0].lastIndexOf('{')
    if (matchingBrace(source, open) >= index) return match[1].replace(/<.*>/, '')
  }
  return null
}

function items(relative, source) {
  const clean = mask(source)
  const result = []
  const pattern = /\b(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^{}()]*>)?\s*\(/g
  for (const match of clean.matchAll(pattern)) {
    const open = clean.indexOf('{', match.index + match[0].length)
    if (open < 0) continue
    let end
    try {
      end = matchingBrace(clean, open)
    } catch (error) {
      throw new Error(`${relative}:${match[1]}: ${error.message}`)
    }
    const owner = ownerAt(clean, match.index)
    const name = owner ? `${owner}::${match[1]}` : match[1]
    const returnType = clean.slice(match.index + match[0].length, open)
      .match(/->\s*([A-Za-z_][A-Za-z0-9_:<>]*)/)?.[1]
      ?.replace(/<.*>/, '') ?? null
    result.push({ key: `${relative}::${name}`, relative, name, returnType, body: clean.slice(open + 1, end) })
  }
  return result
}

function imports(source) {
  return [...mask(source).matchAll(/^\s*use\s+([\s\S]*?);/gm)]
    .map((match) => match[1].replace(/\s+/g, ' ').trim())
}

function importAliases(source) {
  const aliases = new Map()
  for (const declaration of imports(source)) {
    const simple = declaration.match(/^(crate|self|super)(?:::[A-Za-z_][A-Za-z0-9_]*)+(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?$/)
    if (!simple) continue
    const path = declaration.replace(/\s+as\s+[A-Za-z_][A-Za-z0-9_]*$/, '')
    aliases.set(simple[2] ?? path.split('::').at(-1), path)
  }
  return aliases
}

function externalAssociatedPath(name, declarations) {
  if (name.startsWith('oxiroute_rtmp::')) return name
  const match = name.match(/^([A-Z][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)$/)
  if (!match) return null
  const [, owner, method] = match
  const imported = reviewedExternalAssociatedTypes.has(owner) && declarations.some((declaration) =>
    declaration.startsWith('oxiroute_rtmp::{') && new RegExp(`\\b${owner}\\b`).test(declaration))
  const fullyQualifiedCompanion = reviewedExternalAssociatedCalls.has(`oxiroute_rtmp::${owner}::${method}`)
  return imported || fullyQualifiedCompanion ? `oxiroute_rtmp::${owner}::${method}` : null
}

function calls(body) {
  const result = []
  const direct = /(?<![.!])\b([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:::<[^;{}()]*>)?\s*\(/g
  for (const match of body.matchAll(direct)) {
    if (!languageCalls.has(match[1])) result.push({ kind: 'path', name: match[1] })
  }
  result.push(...methodCalls(body))
  for (const match of body.matchAll(/\b([A-Z][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)\b(?!\s*\()/g)) {
    result.push({ kind: 'item', name: match[1] })
  }
  for (const match of body.matchAll(/\blet\s+[A-Za-z_][A-Za-z0-9_]*\s*=\s*([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*;/g)) {
    result.push({ kind: 'item', name: match[1] })
  }
  for (const match of body.matchAll(/\.\s*(?:map|and_then|map_err|filter|filter_map|flat_map|fold|for_each)\s*\(\s*([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*\)/g)) {
    result.push({ kind: 'item', name: match[1] })
  }
  return result
}

function methodCalls(body) {
  const result = []
  const method = /\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:::<[^;{}()]*>)?\s*\(/g
  for (const match of body.matchAll(method)) {
    const receiver = receiverExpression(body, match.index)
    if (!receiver) {
      throw new Error(`unresolved chained method receiver before ${match[1]} near ${body.slice(Math.max(0, match.index - 40), match.index + 40).trim()}`)
    }
    result.push({ kind: 'method', receiver, name: match[1] })
  }
  return result
}

function receiverExpression(body, dot) {
  let index = dot - 1
  while (index >= 0 && /\s/.test(body[index])) index -= 1
  const end = index + 1
  const pairs = new Map([[')', '('], [']', '['], ['}', '{']])
  const opens = new Set(pairs.values())
  const stack = []
  for (; index >= 0; index -= 1) {
    const character = body[index]
    if (pairs.has(character)) {
      stack.push(pairs.get(character))
      continue
    }
    if (opens.has(character)) {
      if (stack.at(-1) === character) {
        stack.pop()
        continue
      }
      if (stack.length === 0) break
    }
    if (stack.length === 0 && /[;,={}?!+*/%&|<>\n]/.test(character)) break
  }
  return body.slice(index + 1, end).trim() || '<masked-literal>'
}

function functionAliases(body) {
  const aliases = new Map()
  const assignment = /(?:\blet\s+(?:mut\s+)?|(?<![.:])\b)([a-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*;/g
  for (const match of body.matchAll(assignment)) aliases.set(match[1], match[2])
  return aliases
}

function closureBindings(body) {
  return new Set([...body.matchAll(/\blet\s+(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*=\s*(?:move\s+)?\|/g)]
    .map((match) => match[1]))
}

function valueTypes(body) {
  const types = new Map()
  for (const match of body.matchAll(/\blet\s+(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*:\s*([A-Za-z_][A-Za-z0-9_:<>]*)/g)) {
    types.set(match[1], match[2].replace(/<.*>/, '').split('::').at(-1))
  }
  for (const match of body.matchAll(/\blet\s+(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*=\s*\[?\s*([A-Z][A-Za-z0-9_]*)\b/g)) {
    types.set(match[1], match[2])
  }
  return types
}

function receiverType(receiver, item, values, byName) {
  let expression = receiver.trim()
  while (expression.startsWith('(') && expression.endsWith(')')) {
    expression = expression.slice(1, -1).trim()
  }
  const arc = expression.match(/^Arc::new\s*\((.*)\)$/s)
  if (arc) return receiverType(arc[1], item, values, byName)
  const constructor = expression.match(/^([A-Z][A-Za-z0-9_]*)::[A-Za-z_][A-Za-z0-9_]*\s*\(/)
  if (constructor) return constructor[1]
  const unit = expression.match(/^([A-Z][A-Za-z0-9_]*)$/)
  if (unit) return unit[1]
  const indexed = expression.match(/^([a-z_][A-Za-z0-9_]*)\s*\[/)
  if (indexed && values.has(indexed[1])) return values.get(indexed[1])
  const variable = expression.match(/^([a-z_][A-Za-z0-9_]*)$/)
  if (variable && values.has(variable[1])) return values.get(variable[1])
  const call = expression.match(/^([A-Za-z_][A-Za-z0-9_:]*)\s*\(/)
  if (call) {
    const candidates = byName.get(call[1]) ?? byName.get(call[1].split('::').at(-1)) ?? []
    const local = candidates.filter((candidate) => compilerModules.has(candidate.relative))
    const sameModule = local.filter((candidate) => candidate.relative === item.relative)
    const target = sameModule.length === 1 ? sameModule[0] : local.length === 1 ? local[0] : null
    return target?.returnType?.split('::').at(-1) ?? null
  }
  return null
}

function verify(sources) {
  const graph = new Map()
  const byName = new Map()
  const aliases = new Map()
  const sourceImports = new Map()
  for (const [relative, source] of sources) {
    sourceImports.set(relative, imports(source))
    aliases.set(relative, importAliases(source))
    for (const item of items(relative, source)) {
      graph.set(item.key, item)
      const short = item.name.split('::').at(-1)
      byName.set(short, [...(byName.get(short) ?? []), item])
      if (item.name !== short) byName.set(item.name, [...(byName.get(item.name) ?? []), item])
    }
  }

  const queue = [...roots]
  const visited = new Set()
  while (queue.length) {
    const key = queue.pop()
    if (visited.has(key)) continue
    const item = graph.get(key)
    if (!item) throw new Error(`unresolved purity root/helper ${key}`)
    visited.add(key)
    const itemAliases = functionAliases(item.body)
    const closures = closureBindings(item.body)
    const values = valueTypes(item.body)
    for (const declaration of sourceImports.get(item.relative)) {
      const importedPath = declaration.replace(/\s+as\s+[A-Za-z_][A-Za-z0-9_]*$/, '')
      if (forbiddenImports.test(importedPath) && !mixedModuleImports.get(item.relative)?.has(declaration)) {
        throw new Error(`${item.relative}: forbidden import ${declaration}`)
      }
    }
    for (const call of calls(item.body)) {
      if (call.kind === 'path' && itemAliases.has(call.name)) call.name = itemAliases.get(call.name)
      const first = call.name.split('::')[0]
      const imported = aliases.get(item.relative).get(first)
      const resolvedName = imported
        ? call.name.replace(first, imported)
        : call.name
      if (acquisitionNames.test(resolvedName)) {
        throw new Error(`${item.relative}:${item.name}: forbidden acquisition call ${resolvedName}`)
      }
      const methodName = resolvedName.split('::').at(-1)
      const receiver = call.kind === 'method'
        ? receiverType(call.receiver, item, values, byName)
        : null
      const qualifiedMethod = receiver
        ? `${receiver}::${methodName}`
        : resolvedName
      if (call.kind !== 'method' && [...reviewedPathCalls].some(([prefix]) => resolvedName.startsWith(prefix))) continue
      if (call.kind === 'method' && !receiver && reviewedValueMethods.has(methodName)) continue
      if (call.kind !== 'method' && reviewedExternalAssociatedCalls.has(resolvedName)) continue
      const externalAssociated = call.kind !== 'method'
        ? externalAssociatedPath(resolvedName, sourceImports.get(item.relative))
        : null
      const exactLocalAssociated = (byName.get(resolvedName) ?? [])
        .some((candidate) => compilerModules.has(candidate.relative))
      if (externalAssociated
        && reviewedExternalAssociatedCalls.has(externalAssociated)
        && !exactLocalAssociated) continue
      const candidates = call.kind === 'method'
        ? receiver
          ? byName.get(qualifiedMethod) ?? []
          : byName.get(methodName) ?? []
        : byName.get(resolvedName) ?? byName.get(methodName) ?? []
      const local = candidates.filter((candidate) => compilerModules.has(candidate.relative))
      if (local.length === 1) {
        queue.push(local[0].key)
        continue
      }
      if (local.length > 1) {
        const sameModule = local.filter((candidate) => candidate.relative === item.relative)
        if (sameModule.length === 1) {
          queue.push(sameModule[0].key)
          continue
        }
        throw new Error(`${item.relative}:${item.name}: ambiguous local call ${resolvedName}`)
      }
      if (call.kind === 'method' && reviewedValueMethods.has(methodName)) continue
      if (call.kind === 'path' && closures.has(resolvedName)) continue
      if (call.kind === 'item' && /^[A-Z]/.test(methodName)) {
        continue // Reviewed enum/associated constant value; not an invocation.
      }
      if (call.kind === 'path' && resolvedName.includes('::') && /^[A-Z][A-Za-z0-9_]*$/.test(methodName)) {
        continue // Reviewed Rust enum/tuple-struct value constructor; local functions were resolved first.
      }
      if (/^(crate|self|super)::/.test(resolvedName)) {
        throw new Error(`${item.relative}:${item.name}: unresolved first-party call ${resolvedName}`)
      }
      throw new Error(`${item.relative}:${item.name}: unresolved call ${resolvedName}`)
    }
  }
  return visited
}

const sources = new Map([...compilerModules].map((relative) => [
  relative,
  fs.readFileSync(path.join(sourceRoot, relative), 'utf8'),
]))
const visited = verify(sources)

function mustFail(label, mutate) {
  const candidate = new Map(sources)
  mutate(candidate)
  try { verify(candidate) } catch { return }
  throw new Error(`mutation self-test did not fail: ${label}`)
}

function mustPass(label, mutate) {
  const candidate = new Map(sources)
  mutate(candidate)
  try { verify(candidate) } catch (error) {
    throw new Error(`mutation self-test unexpectedly failed: ${label}: ${error.message}`)
  }
}

const injection = 'let config = validated.as_draft();'
const mutateRoot = (mutate) => (candidate) => candidate.set('generation_compiler.rs', mutate(candidate.get('generation_compiler.rs')))
mustFail('direct acquisition', mutateRoot((source) => source.replace(injection, `${injection} std::fs::read("secret").unwrap();`)))
mustFail('From function item acquisition', mutateRoot((source) => source.replace(injection, `${injection} let f = Impure::from; f(config);`).concat('\nstruct Impure; impl Impure { fn from(_: &oxiroute_config::ConfigDraft) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('lowercase function item acquisition', mutateRoot((source) => source.replace(injection, `${injection} let f = impure_helper; f();`).concat('\nfn impure_helper() { std::fs::read("secret").unwrap(); }\n')))
mustFail('let mut alias acquisition', mutateRoot((source) => source.replace(injection, `${injection} let mut f = impure_helper; f();`).concat('\nfn impure_helper() { std::fs::read("secret").unwrap(); }\n')))
mustFail('reassigned alias acquisition', mutateRoot((source) => source.replace(injection, `${injection} let mut f = pure_helper; f = impure_helper; f();`).concat('\nfn pure_helper() {} fn impure_helper() { std::fs::read("secret").unwrap(); }\n')))
mustFail('callback function item acquisition', mutateRoot((source) => source.replace(injection, `${injection} [()].into_iter().map(impure_helper).collect::<Vec<_>>();`).concat('\nfn impure_helper(_: ()) { std::fs::read("secret").unwrap(); }\n')))
mustFail('callback parameter is unresolved', mutateRoot((source) => source.replace(injection, `${injection} invoke_callback(impure_helper);`).concat('\nfn invoke_callback(callback: fn()) { callback(); } fn impure_helper() { std::fs::read("secret").unwrap(); }\n')))
mustFail('inherent method acquisition', mutateRoot((source) => source.replace(injection, `${injection} Impure.acquire();`).concat('\nstruct Impure; impl Impure { fn acquire(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('closure acquisition', mutateRoot((source) => source.replace(injection, `${injection} let impure = || std::fs::read("secret").unwrap(); impure();`)))
mustFail('indirect helper acquisition', mutateRoot((source) => source.replace(injection, `${injection} impure_helper();`).concat('\nfn impure_helper() { std::fs::read("secret").unwrap(); }\n')))
mustFail('acquisition import', mutateRoot((source) => `use std::fs;\n${source}`))
mustFail('aliased acquisition import', mutateRoot((source) => `use std::fs as disk;\n${source.replace(injection, `${injection} disk::read("secret").unwrap();`)}`))
mustFail('reached helper module import', (candidate) => {
  candidate.set('routing.rs', `use std::fs as disk;\n${candidate.get('routing.rs')}`)
})
mustFail('renamed import in indirect module', (candidate) => {
  candidate.set('routing.rs', `use std::fs as renamed_disk;\n${candidate.get('routing.rs').replace('fn compile_passive_health(', 'fn compile_passive_health(').replace('{', '{ renamed_disk::read("secret").unwrap();', 1)}`)
})
mustFail('receiver ambiguity acquisition', mutateRoot((source) => source.replace(injection, `${injection} Impure.parse();`).concat('\nstruct Impure; impl Impure { fn parse(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('constructor chained receiver acquisition', mutateRoot((source) => source.replace(injection, `${injection} Impure::new().run();`).concat('\nstruct Impure; impl Impure { fn new() -> Self { Self } fn run(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('call chained receiver acquisition', mutateRoot((source) => source.replace(injection, `${injection} make_impure().run();`).concat('\nstruct Impure; fn make_impure() -> Impure { Impure } impl Impure { fn run(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('parenthesized chained receiver acquisition', mutateRoot((source) => source.replace(injection, `${injection} (Impure).run();`).concat('\nstruct Impure; impl Impure { fn run(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('indexed chained receiver acquisition', mutateRoot((source) => source.replace(injection, `${injection} let items = [Impure]; items[0].run();`).concat('\nstruct Impure; impl Impure { fn run(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('Arc constructor chained receiver acquisition', mutateRoot((source) => source.replace(injection, `${injection} Arc::new(Impure).run();`).concat('\nstruct Impure; impl Impure { fn run(&self) { std::fs::read("secret").unwrap(); } }\n')))
mustFail('local constructor matching reviewed external associated call', mutateRoot((source) => source.replace(injection, `${injection} RtmpAccessRulePlan::new();`).concat('\nstruct RtmpAccessRulePlan; impl RtmpAccessRulePlan { fn new() -> Self { std::fs::read("secret").unwrap(); Self } }\n')))
mustPass('reviewed pure constructor chain', mutateRoot((source) => source.replace(injection, `${injection} Arc::new(config).as_ref();`)))
mustFail('unresolved first-party call', mutateRoot((source) => source.replace(injection, `${injection} crate::missing::helper();`)))

console.log(`Generation compiler purity graph verified (${visited.size} reachable items); strict resolution mutation self-tests passed`)
