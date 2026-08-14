#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const crate = path.join(root, 'crates/oxiroute-rtmp')
const checked = new Map([
  ['src/composition.rs', {
    std: new Set(['fmt', 'net::SocketAddr', 'path::{Path, PathBuf}', 'sync::Arc', 'time::Duration']),
    external: new Set(),
    modules: new Set(),
  }],
])
const allowedCrateSymbols = new Set([
  'DashOutputConfig', 'DashSegmentNaming', 'DestinationPolicyError', 'ExecEnvironment', 'ExecFilesystemPolicy',
  'ExecLimits', 'ExecMode', 'ExecNetworkPolicy', 'ExecProfile', 'ExecProfileError', 'ExecTrigger',
  'HlsFragmentNaming', 'HlsKeyConfig', 'HlsOutputConfig', 'HlsValueError', 'HlsVariant', 'LiveHub',
  'LiveHubLimits', 'MediaApplication', 'MediaStore',
  'RecorderWorkerConfig',
  'RecorderWorkerStartError', 'RecordingPathPolicy', 'RecordingStoreLimits',
  'RecordingStoreLimitsError', 'RtmpAccessAction', 'RtmpAccessPolicy', 'RtmpAccessRule', 'RtmpAutoPushConfig',
  'RtmpApplication', 'RtmpAutoPushConfigError', 'RtmpCallbackMethod', 'RtmpCallbackPolicy',
  'RtmpCallbackValueError', 'RtmpNetwork', 'RtmpOutboundPolicy', 'RtmpPullTarget',
  'RtmpPushApplication', 'RtmpPushTarget', 'RtmpRecorderPolicy', 'RtmpRecorderStart',
  'RtmpSessionCeilings', 'RtmpSessionLimitError', 'RtmpSessionLimits', 'RtmpStreamPath',
  'RtmpTokenPolicy', 'RtmpTransport', 'VodApplication', 'VodLimits', 'VodSourceDefinition',
  'VodValueError',
  'validate_callback_url_intrinsic',
])
const allowedCargoDependencies = new Set([
  'bytes', 'chrono', 'chrono-tz', 'http', 'openssl', 'rml_rtmp', 'rustix', 'sha2', 'thiserror',
  'uuid',
])

function splitTopLevel(value) {
  const values = []
  let depth = 0
  let start = 0
  for (let index = 0; index < value.length; index += 1) {
    if (value[index] === '{') depth += 1
    if (value[index] === '}') depth -= 1
    if (value[index] === ',' && depth === 0) {
      values.push(value.slice(start, index).trim())
      start = index + 1
    }
  }
  values.push(value.slice(start).trim())
  return values.filter(Boolean)
}

function imports(source) {
  return [...source.matchAll(/^use\s+([\s\S]*?);$/gm)].map((match) =>
    match[1].replace(/\s+/g, ' ').trim(),
  )
}

function members(value) {
  const open = value.indexOf('{')
  if (open === -1) return [value]
  const prefix = value.slice(0, open)
  return splitTopLevel(value.slice(open + 1, value.lastIndexOf('}'))).map((item) =>
    item.includes('{') ? `${prefix}${item}` : item,
  )
}

for (const [relative, policy] of checked) {
  const source = fs.readFileSync(path.join(crate, relative), 'utf8')
  const declaredModules = [...source.matchAll(/^mod\s+([A-Za-z0-9_]+)\s*;/gm)].map((match) => match[1])
  for (const moduleName of declaredModules) {
    if (!policy.modules.has(moduleName)) throw new Error(`${relative}: unapproved module ${moduleName}`)
  }
  for (const declaration of imports(source)) {
    if (declaration.startsWith('std::')) {
      for (const item of members(declaration.slice(5))) {
        if (!policy.std.has(item)) throw new Error(`${relative}: unapproved std import ${item}`)
      }
      continue
    }
    if (declaration.startsWith('crate::')) {
      for (const item of members(declaration.slice(7))) {
        if (!allowedCrateSymbols.has(item)) {
          throw new Error(`${relative}: unapproved crate import ${item}`)
        }
      }
      continue
    }
    const dependency = declaration.split('::', 1)[0]
    if (!policy.external.has(dependency)) {
      throw new Error(`${relative}: unapproved external import ${dependency}`)
    }
  }
}

const cargo = fs.readFileSync(path.join(crate, 'Cargo.toml'), 'utf8')
const dependencies = cargo
  .split('[dependencies]')[1]
  .split('[dev-dependencies]')[0]
  .split('\n')
  .map((line) => line.match(/^([A-Za-z0-9_-]+)\s*=/)?.[1])
  .filter(Boolean)
for (const dependency of dependencies) {
  if (!allowedCargoDependencies.has(dependency)) {
    throw new Error(`Cargo.toml: unapproved RTMP dependency ${dependency}`)
  }
}

console.log('RTMP plan purity import and dependency allowlists verified')
