import { shallowRef } from 'vue'

export type ToastTone = 'ok' | 'info' | 'warn' | 'bad'

export interface ToastMessage {
  id: number
  tone: ToastTone
  text: string
}

export const toasts = shallowRef<readonly ToastMessage[]>([])

let nextId = 1

export function dismissToast(id: number): void {
  toasts.value = toasts.value.filter((toast) => toast.id !== id)
}

export function pushToast(text: string, tone: ToastTone = 'info', ttlMs?: number): number {
  const id = nextId++
  toasts.value = [...toasts.value, { id, tone, text }]
  if (ttlMs !== undefined) setTimeout(() => dismissToast(id), ttlMs)
  return id
}

export function clearToasts(): void {
  toasts.value = []
}
