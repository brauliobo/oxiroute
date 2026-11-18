import { watchPostEffect, type ComputedRef, type Ref } from 'vue'

import type { PlacedDiagnostic } from './context'

const CONTROLS = 'input, select, textarea'
const WIRED = 'data-issue-wired'

function foreignDescriptions(target: Element): string[] {
  return (target.getAttribute('aria-describedby') ?? '').split(' ').filter((id) => id && !id.startsWith('config-issue-'))
}

function unwire(target: Element, group: boolean): void {
  if (!target.hasAttribute(WIRED)) return
  const rest = foreignDescriptions(target)
  target.removeAttribute(WIRED)
  target.removeAttribute('aria-invalid')
  if (rest.length === 0) target.removeAttribute('aria-describedby')
  else target.setAttribute('aria-describedby', rest.join(' '))
  if (group) target.removeAttribute('tabindex')
}

export function useIssueWiring(root: Ref<HTMLElement | null>, issues: ComputedRef<PlacedDiagnostic[]>, group = false) {
  watchPostEffect(() => {
    const element = root.value
    const target = group ? element : element?.querySelector(CONTROLS)
    if (!target) return
    const ids = issues.value.map(({ id }) => id)
    if (ids.length === 0) return unwire(target, group)
    target.setAttribute(WIRED, '')
    target.setAttribute('aria-invalid', 'true')
    target.setAttribute('aria-describedby', [...foreignDescriptions(target), ...ids].join(' '))
    if (group) target.setAttribute('tabindex', '-1')
  })
}
