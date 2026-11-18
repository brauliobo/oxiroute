export type Politeness = 'polite' | 'assertive'

const regions = new Map<Politeness, HTMLElement>()

function region(politeness: Politeness): HTMLElement {
  const existing = regions.get(politeness)
  if (existing?.isConnected) return existing
  const element = document.createElement('div')
  element.className = 'ox-sr-only'
  element.setAttribute('role', politeness === 'polite' ? 'status' : 'alert')
  element.setAttribute('aria-live', politeness)
  element.setAttribute('aria-atomic', 'true')
  document.body.append(element)
  regions.set(politeness, element)
  return element
}

export function useLiveRegion(): { announce(message: string, politeness?: Politeness): void } {
  return {
    announce(message, politeness = 'polite') {
      region(politeness).textContent = message
    },
  }
}
