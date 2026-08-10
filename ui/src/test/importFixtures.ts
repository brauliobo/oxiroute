import type {
  ImportReportEnvelope,
  ImportReportResponse,
} from '../api'
import { contractConfigSnapshot } from './contractFixtures'

const diskRevision = 'disk-import-111111111111111111111111111111111111111111111111111111111111'
const candidateRevision = 'candidate-import-2222222222222222222222222222222222222222222222222222222'

export function importReportResponse(blocked = false): ImportReportResponse {
  const report: ImportReportEnvelope = {
    schemaVersion: 1,
    source: {
      product: 'apache',
      version: null,
      versionSource: null,
      capabilityProfile: { id: 'apache-static-reverse-proxy', version: 1 },
    },
    sourceGraph: {
      roots: [{ ordinal: 0, path: '<redacted>', sourceIds: [1], outcome: 'loaded' }],
      sources: [
        { id: 1, name: 'source-1', path: '<redacted>', byteLength: 128, fingerprintSha256: 'a'.repeat(64) },
        { id: 2, name: 'source-2', path: '<redacted>', byteLength: 256, fingerprintSha256: 'b'.repeat(64) },
      ],
      dependencies: [{
        sourceId: 1,
        targetSourceId: 2,
        kind: 'include',
        requestedPath: '<redacted>',
        canonicalPath: '<redacted>',
        optional: false,
        status: 'expanded',
        span: { sourceId: 1, range: { start: 0, end: 20 } },
        failureCode: null,
        fingerprintSha256: 'b'.repeat(64),
        truncated: false,
      }],
      dependenciesComplete: true,
      snapshotStable: true,
    },
    sourceMetadata: {
      environmentFingerprintSha256: null,
      inactiveSources: [],
      originalSourceIds: [1, 2],
      sourceMaps: [],
    },
    candidate: {
      ...(blocked
        ? { finalized: false as const, config: null }
        : { finalized: true as const, config: structuredClone(contractConfigSnapshot().config) }),
      draft: {
        version: 1,
        maxConnections: null,
        management: true,
        stats: false,
        certificates: 1,
        tlsProfiles: 1,
        listeners: 1,
        upstreamPools: 1,
        httpServices: 1,
        cacheStores: 0,
        forwardProxyServices: 0,
        rtmpServices: 0,
        l4Services: 0,
      },
      provenance: [{
        path: '/http_services/0/routes/0/action',
        origins: [{
          role: 'proxy_pass',
          sourceId: 2,
          range: { start: 44, end: 78 },
          path: '<redacted>',
          line: 3,
          includeStack: [{ sourceId: 1, range: { start: 0, end: 20 } }],
        }],
      }],
    },
    blockers: blocked ? [{
      id: 'apache-vhost:blocked:rewrite',
      kind: 'virtual_host',
      code: 'E_REWRITE_UNSUPPORTED',
      message: 'Rewrite behavior is outside the exact canonical subset.',
      scope: 'blocked.example.test',
      occurrenceIds: [],
      origins: [{ sourceId: 2, range: { start: 80, end: 96 } }],
    }] : [],
    requirements: {
      deployment: [{
        kind: 'certificate_material',
        directive: 'SSLCertificateFile',
        values: ['/etc/ssl/certs/site.pem'],
        origins: [{ role: 'certificate', sourceId: 2, range: { start: 100, end: 120 }, path: '<redacted>', line: 5, includeStack: [] }],
        equivalentRuntimeEndpoint: null,
      }],
      activation: [{
        kind: 'listener_mode',
        directive: 'Listen',
        values: [],
        origins: [{ role: 'listener', sourceId: 1, range: { start: 0, end: 10 }, path: null, line: 1, includeStack: [] }],
        equivalentRuntimeEndpoint: false,
      }],
    },
    overlays: [{
      id: 'certificate-material',
      kind: 'certificate_material',
      origin: null,
      redactedEvidence: true,
      values: ['private-key-secret'],
      satisfied: true,
    }],
    diagnostics: blocked ? [{
      code: 'E_REWRITE_UNSUPPORTED',
      severity: 'error',
      stage: 'lowering',
      message: 'Rewrite behavior is outside the exact canonical subset.',
      primarySpan: { sourceId: 2, range: { start: 80, end: 96 } },
      includeStack: [],
      relatedSpans: [],
      help: 'Use an explicitly supported canonical route action.',
    }] : [],
  }
  return {
    schemaVersion: 1,
    diskRevision,
    candidateRevision,
    activeRevision: 'active-import-333333333333333333333333333333333333333333333333333333333333',
    configFormat: 'kdl',
    compositional: true,
    reports: [{
      index: 0,
      product: 'apache',
      version: null,
      versionSource: null,
      capabilityProfile: report.source.capabilityProfile,
      status: blocked ? 'blocked' : 'finalized',
      rootCount: 1,
      sourceCount: 2,
      dependencyCount: 1,
      blockerCount: report.blockers.length,
      diagnosticCount: report.diagnostics.length,
      provenanceCount: report.candidate.provenance.length,
      requirementCount: 2,
      overlayCount: 1,
      previewAvailable: !blocked,
    }],
    selection: { index: 0 },
    report,
    preview: blocked ? null : { format: 'kdl', text: 'version 1\n' },
    diagnostics: [],
  }
}

export const importReportDiskRevision = diskRevision
