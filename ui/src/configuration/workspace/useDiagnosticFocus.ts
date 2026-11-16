import { nextTick, type Ref } from 'vue'

function selectionForPath(path: string): string {
  const indexedObject = path.match(/^([a-z_]+)\[(\d+)\]/) ?? path.match(/^\/([a-z_]+)\/(\d+)/)
  if (indexedObject) return `${indexedObject[1]}:${indexedObject[2]}`
  if (path.startsWith('management') || path.startsWith('/management')) return 'management'
  if (path.startsWith('stats') || path.startsWith('/stats')) return 'stats'
  return 'general'
}

function fieldPathFor(path: string): string {
  return path.startsWith('/')
    ? path.slice(1).split('/').reduce((result, segment) =>
        /^\d+$/.test(segment) ? `${result}[]` : `${result}${result ? '.' : ''}${segment}`,
      '')
    : path.replace(/\[\d+\]/g, '[]')
}

export function useDiagnosticFocus(workspaceRoot: Ref<HTMLElement | null>, selectedKey: Ref<string>) {
  return async function focusDiagnostic(path: string): Promise<void> {
    selectedKey.value = selectionForPath(path)
    const fieldPath = fieldPathFor(path)
    await nextTick()
    const field = Array.from(workspaceRoot.value?.querySelectorAll<HTMLElement>('[data-field]') ?? [])
      .find((candidate) => candidate.dataset.field === fieldPath)
    const control = field?.matches('input, select, textarea, button')
      ? field
      : field?.querySelector<HTMLElement>('input, select, textarea, button')
    control?.focus()
    field?.scrollIntoView?.({ block: 'center' })
  }
}
