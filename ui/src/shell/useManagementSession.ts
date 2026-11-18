import { ref } from 'vue'

export const SESSION_TOKEN_KEY = 'oxiroute.management-token'

type TokenStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

export function useManagementSession(storage: TokenStorage = sessionStorage) {
  const token = ref(storage.getItem(SESSION_TOKEN_KEY) ?? '')
  const rejected = ref(false)
  const epoch = ref(0)

  function signIn(next: string): void {
    token.value = next
    rejected.value = false
    storage.setItem(SESSION_TOKEN_KEY, next)
  }

  function clear(): void {
    token.value = ''
    storage.removeItem(SESSION_TOKEN_KEY)
  }

  function signOut(): void {
    clear()
    rejected.value = false
    epoch.value += 1
  }

  function reject(): void {
    clear()
    rejected.value = true
  }

  return { token, rejected, epoch, signIn, signOut, reject }
}
