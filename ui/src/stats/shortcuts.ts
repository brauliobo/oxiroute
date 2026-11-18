import { shortcutFor } from '#design/composables/useShortcuts'

export const SEARCH_ID = 'ox-search'

export type ShortcutAction = 'search' | 'help' | 'pause' | 'refresh' | 'theme' | 'problems'

export const KEYMAP: Readonly<Record<string, ShortcutAction>> = {
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

export const shortcutAction = (event: KeyboardEvent): ShortcutAction | undefined => shortcutFor(event, KEYMAP)
