import { afterEach } from 'vitest'
import { SESSION_TOKEN_KEY } from '../shell/useManagementSession'

export const sessionToken = 'test-only-session-token'

afterEach(() => sessionStorage.clear())

export function signIn(token = sessionToken): void {
  sessionStorage.setItem(SESSION_TOKEN_KEY, token)
}
