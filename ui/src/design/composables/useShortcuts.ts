import { onMounted, onScopeDispose } from 'vue'

export type Keymap<Action extends string> = Readonly<Record<string, Action>>

const TYPING = 'input, textarea, select, [contenteditable]'

export function shortcutFor<Action extends string>(event: KeyboardEvent, keymap: Keymap<Action>): Action | undefined {
  if (event.ctrlKey || event.metaKey || event.altKey || event.repeat) return undefined
  const target = event.target instanceof Element ? event.target : undefined
  if (target?.closest(`${TYPING}, dialog`)) return undefined
  return keymap[event.key.toLowerCase()]
}

export function useShortcuts<Action extends string>(
  keymap: Keymap<Action>,
  handlers: Readonly<Record<Action, () => void>>,
): void {
  const onKeydown = (event: KeyboardEvent) => {
    const action = shortcutFor(event, keymap)
    if (!action) return
    event.preventDefault()
    handlers[action]()
  }
  onMounted(() => document.addEventListener('keydown', onKeydown))
  onScopeDispose(() => document.removeEventListener('keydown', onKeydown))
}
