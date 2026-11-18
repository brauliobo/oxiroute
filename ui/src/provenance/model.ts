import type { Tone } from '#design/badge/tone'
import type { SelectOption } from '#design/field/types'
import { formatBytes } from '#design/format'
import type {
  ImportReportEnvelope,
  ImportReportOrigin,
  ImportReportRequirement,
  ImportReportSummary,
} from '../api'
import { sentence } from '../events/format'
import { plural } from '../workspace/plural'
import type { Fact } from '../workspace/types'

type Rows = string[][]

const STATUS_BADGES: Readonly<Record<ImportReportSummary['status'], { tone: Tone; label: string }>> = {
  finalized: { tone: 'ok', label: 'Finalized' },
  blocked: { tone: 'bad', label: 'Blocked' },
  draft: { tone: 'warn', label: 'Draft' },
}

export const statusBadge = (status: ImportReportSummary['status']): { tone: Tone; label: string } => STATUS_BADGES[status]

export const selectionOptions = (reports: readonly ImportReportSummary[]): SelectOption[] =>
  reports.map((summary) => ({
    value: String(summary.index),
    label: `${summary.product} / ${summary.capabilityProfile.id} / ${summary.status}`,
  }))

export const requirementsOf = (report: ImportReportEnvelope): ImportReportRequirement[] =>
  [...report.requirements.deployment, ...report.requirements.activation]

export const originLabel = (origin: ImportReportOrigin): string => `${origin.role ?? 'source origin'} / source ${origin.sourceId}`

export const versionText = (report: ImportReportEnvelope): string =>
  report.source.version ? `Source version ${report.source.version}` : 'Source version not supplied'

export function reportMetrics(report: ImportReportEnvelope): Fact[] {
  return [
    { label: 'Sources', value: String(report.sourceGraph.sources.length) },
    { label: 'Dependencies', value: String(report.sourceGraph.dependencies.length) },
    { label: 'Provenance paths', value: String(report.candidate.provenance.length) },
    { label: 'Requirements', value: String(requirementsOf(report).length) },
    { label: 'Diagnostics', value: String(report.diagnostics.length) },
  ]
}

export const sourceRows = (report: ImportReportEnvelope): Rows =>
  report.sourceGraph.sources.map((source) => [`Source ${source.id}`, source.name, formatBytes(source.byteLength)])

export const dependencyRows = (report: ImportReportEnvelope): Rows =>
  report.sourceGraph.dependencies.map((dependency) => [
    dependency.kind,
    `Source ${dependency.sourceId}`,
    dependency.targetSourceId === null ? 'Unresolved' : `Source ${dependency.targetSourceId}`,
    `${sentence(dependency.status)}${dependency.truncated ? ', truncated' : ''}`,
  ])

export const blockerRows = (report: ImportReportEnvelope): Rows =>
  report.blockers.map((blocker) => [blocker.code, blocker.message, blocker.scope || blocker.kind])

export const requirementRows = (report: ImportReportEnvelope): Rows =>
  requirementsOf(report).map((requirement) => [
    requirement.directive,
    sentence(requirement.kind),
    requirement.values.length ? `${plural(requirement.values.length, 'value')} redacted` : 'No values supplied',
  ])

export const originRows = (report: ImportReportEnvelope): Rows =>
  report.candidate.provenance.map((entry) => [
    entry.path,
    plural(entry.origins.length, 'origin'),
    entry.origins[0] ? originLabel(entry.origins[0]) : '-',
  ])

export const overlayRows = (report: ImportReportEnvelope): Rows =>
  report.overlays.map((overlay) => [overlay.id, sentence(overlay.kind), overlay.satisfied ? 'Satisfied' : 'Not satisfied'])

export const diagnosticRows = (report: ImportReportEnvelope): Rows =>
  report.diagnostics.map((diagnostic) => [diagnostic.code, diagnostic.message, sentence(diagnostic.stage), sentence(diagnostic.severity)])
