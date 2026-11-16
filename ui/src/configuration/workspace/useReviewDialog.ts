import { nextTick, ref, type Ref } from 'vue'

export function useReviewDialog(workspaceRoot: Ref<HTMLElement | null>) {
  const reviewOpen = ref(false)
  let returnFocus: HTMLElement | null = null

  function setBackgroundInert(inert: boolean): void {
    const workspace = workspaceRoot.value
    const parent = workspace?.parentElement
    if (!workspace || !parent) return
    for (const sibling of parent.children) {
      if (sibling !== workspace) sibling.toggleAttribute('inert', inert)
    }
  }

  function enterReview(event?: Event): void {
    returnFocus = event?.currentTarget instanceof HTMLElement
      ? event.currentTarget
      : document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null
    reviewOpen.value = true
    setBackgroundInert(true)
    void nextTick(() =>
      document.querySelector<HTMLButtonElement>('.save-review .close-button')?.focus(),
    )
  }

  function exitReview(fallbackSelector?: string): void {
    if (!reviewOpen.value) return
    reviewOpen.value = false
    setBackgroundInert(false)
    void nextTick(() => {
      const returnTarget = returnFocus
      returnFocus = null
      if (
        returnTarget?.isConnected &&
        !(returnTarget instanceof HTMLButtonElement && returnTarget.disabled)
      ) {
        returnTarget.focus()
        return
      }
      document.querySelector<HTMLElement>(fallbackSelector ?? '.review-button')?.focus()
    })
  }

  function releaseBackground(): void {
    setBackgroundInert(false)
  }

  return { reviewOpen, enterReview, exitReview, releaseBackground }
}
