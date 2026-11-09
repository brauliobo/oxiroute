// Keyboard shortcuts of the page. Single keys, so they never act while the viewer types, behind a dialog or with a
// browser chord (Ctrl, Alt, Meta). `shortcutAction` is the whole policy; the shell runs the action.

export const SEARCH_ID = 'ox-search'

export type ShortcutAction = 'search' | 'help' | 'pause' | 'refresh' | 'theme' | 'problems'

const ACTIONS: Readonly<Record<string, ShortcutAction>> = {
  '/': 'search', '?': 'help', p: 'pause', r: 'refresh', t: 'theme', f: 'problems',
}

/** What the help dialog lists. */
export const SHORTCUTS: ReadonlyArray<{ keys: string; description: string }> = [
  { keys: '/', description: 'Focus the search box.' },
  { keys: '?', description: 'Show this list of shortcuts.' },
  { keys: 'p', description: 'Pause or resume the automatic refresh.' },
  { keys: 'r', description: 'Refresh now, also while paused.' },
  { keys: 't', description: 'Switch the theme: automatic, light, dark.' },
  { keys: 'f', description: 'Show only rows that need attention, or all rows.' },
  { keys: 'Esc', description: 'Clear the search box, or close a dialog.' },
]

const TYPING = 'input, textarea, select, [contenteditable]'

export function shortcutAction(event: KeyboardEvent): ShortcutAction | undefined {
  if (event.ctrlKey || event.metaKey || event.altKey || event.repeat) return undefined
  const target = event.target instanceof Element ? event.target : undefined
  if (target?.closest(`${TYPING}, dialog`)) return undefined
  return ACTIONS[event.key.toLowerCase()]
}
