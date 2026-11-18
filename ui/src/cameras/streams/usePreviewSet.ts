import { ref } from 'vue'

export const MAX_PREVIEWS = 4

export function usePreviewSet(limit = MAX_PREVIEWS) {
  const active = ref<readonly string[]>([])

  const has = (id: string) => active.value.includes(id)

  function toggle(id: string): void {
    active.value = has(id) ? active.value.filter((other) => other !== id) : [...active.value, id].slice(-limit)
  }

  return { active, has, toggle }
}
