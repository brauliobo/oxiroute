import { shallowRef } from 'vue'

export interface ConfirmRequest {
  title: string
  message: string
  confirmLabel: string
  details?: readonly string[]
  danger?: boolean
  typeToConfirm?: string
}

interface PendingConfirm extends ConfirmRequest {
  settle: (confirmed: boolean) => void
}

export const pendingConfirm = shallowRef<PendingConfirm | null>(null)

export function confirmAction(request: ConfirmRequest): Promise<boolean> {
  pendingConfirm.value?.settle(false)
  return new Promise((resolve) => {
    pendingConfirm.value = { ...request, settle: resolve }
  })
}

export function settleConfirm(confirmed: boolean): void {
  const pending = pendingConfirm.value
  pendingConfirm.value = null
  pending?.settle(confirmed)
}
