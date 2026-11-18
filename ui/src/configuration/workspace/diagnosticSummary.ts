import type { ConfigDiagnostic } from '../../config'

const plural = (count: number, noun: string): string => `${count} ${noun}${count === 1 ? '' : 's'}`

export function summarizeDiagnostics(diagnostics: ConfigDiagnostic[]): string {
  const errors = diagnostics.filter(({ severity }) => severity === 'error').length
  const warnings = diagnostics.length - errors
  if (diagnostics.length === 0) return 'No diagnostics'
  return [errors > 0 && plural(errors, 'error'), warnings > 0 && plural(warnings, 'warning')]
    .filter(Boolean)
    .join(', ')
}

export const hasErrors = (diagnostics: ConfigDiagnostic[]): boolean =>
  diagnostics.some(({ severity }) => severity === 'error')
