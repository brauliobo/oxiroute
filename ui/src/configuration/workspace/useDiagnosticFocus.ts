import { nextTick, type Ref } from 'vue'

import type { PlacedDiagnostic } from '../fields/context'

export function useDiagnosticFocus(workspaceRoot: Ref<HTMLElement | null>, selectedKey: Ref<string>) {
  return async function focusDiagnostic({ id, location }: PlacedDiagnostic): Promise<void> {
    if (!location) return
    selectedKey.value = location.selection
    await nextTick()
    const target = workspaceRoot.value?.querySelector<HTMLElement>(`[aria-describedby~="${id}"]`)
    target?.focus()
    target?.scrollIntoView?.({ block: 'center' })
  }
}
