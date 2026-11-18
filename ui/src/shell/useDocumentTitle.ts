import { watchEffect } from 'vue'

export function useDocumentTitle(title: () => string): void {
  watchEffect(() => {
    document.title = title()
  })
}
