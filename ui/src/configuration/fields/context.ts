import { computed, inject, provide, type ComputedRef, type InjectionKey, type Ref } from 'vue'

import type { ConfigDiagnostic } from '../../config'
import type { DiagnosticLocation } from '../../config'

export interface PlacedDiagnostic {
  id: string
  diagnostic: ConfigDiagnostic
  location: DiagnosticLocation | null
}

export interface FieldDiagnostics {
  placed: Ref<PlacedDiagnostic[]>
  selectedKey: Ref<string>
}

const diagnosticsKey: InjectionKey<FieldDiagnostics> = Symbol('configuration field diagnostics')
const scopeKey: InjectionKey<number[]> = Symbol('configuration item scope')

export const issueId = (index: number): string => `config-issue-${index}`

export function provideFieldDiagnostics(value: FieldDiagnostics): void {
  provide(diagnosticsKey, value)
}

export function provideItemScope(index: () => number): void {
  const parent = inject(scopeKey, [])
  provide(scopeKey, [...parent, index()])
}

export function useFieldIssues(path: () => string): ComputedRef<PlacedDiagnostic[]> {
  const context = inject(diagnosticsKey, null)
  const scope = inject(scopeKey, [])
  return computed(() => context === null ? [] : context.placed.value.filter(({ location }) =>
    location !== null &&
    location.field === path() &&
    location.selection === context.selectedKey.value &&
    location.nested.length === scope.length &&
    location.nested.every((index, depth) => index === scope[depth]),
  ))
}
