import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import App from '../App.vue'
import { contractMonitoring, emptyConfigSnapshot, jsonResponse } from '../test/contractFixtures'
import { flushPromises, mount } from '../test/mount'
import { sessionToken } from '../test/session'
import { SESSION_TOKEN_KEY, useManagementSession } from './useManagementSession'

const fakeStorage = () => {
  const items = new Map<string, string>()
  return {
    items,
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
    removeItem: (key: string) => void items.delete(key),
  }
}

describe('management session', () => {
  it('starts signed out and remembers a token only in the given storage', () => {
    const storage = fakeStorage()
    const session = useManagementSession(storage)
    expect(session.token.value).toBe('')
    session.signIn('abc')
    expect(session.token.value).toBe('abc')
    expect(storage.items.get(SESSION_TOKEN_KEY)).toBe('abc')
    expect(useManagementSession(storage).token.value).toBe('abc')
  })

  it('tells signing out from a rejected token and forgets the token in both', () => {
    const storage = fakeStorage()
    const session = useManagementSession(storage)
    session.signIn('abc')
    session.reject()
    expect(session.token.value).toBe('')
    expect(session.rejected.value).toBe(true)
    expect(storage.items.size).toBe(0)
    session.signIn('def')
    expect(session.rejected.value).toBe(false)
    session.signOut()
    expect(session.rejected.value).toBe(false)
    expect(storage.items.size).toBe(0)
  })
})

describe('console login', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse(contractMonitoring()))))
  })

  afterEach(() => {
    window.location.hash = ''
    vi.unstubAllGlobals()
    sessionStorage.clear()
    localStorage.clear()
  })

  it('asks for the token once and sends no request before it exists', async () => {
    const wrapper = mount(App)
    await flushPromises()
    expect(fetch).not.toHaveBeenCalled()
    expect(wrapper.findAll('input[type="password"]')).toHaveLength(1)
    expect(wrapper.get('[role="status"]').text()).toContain('No management token')
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(wrapper.find('.ox-readouts').exists()).toBe(false)
    expect(wrapper.findAll('main')).toHaveLength(1)
  })

  it('opens the console with the token kept for the session only', async () => {
    const wrapper = mount(App)
    await wrapper.get('#management-access-token').setValue(sessionToken)
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith('/api/v1/monitoring', expect.objectContaining({
      headers: { Authorization: `Bearer ${sessionToken}` },
    }))
    expect(sessionStorage.getItem(SESSION_TOKEN_KEY)).toBe(sessionToken)
    expect(localStorage.length).toBe(0)
    expect(window.location.href).not.toContain(sessionToken)
    expect(wrapper.text()).not.toContain(sessionToken)
    expect(wrapper.find('input[type="password"]').exists()).toBe(false)
    expect(wrapper.find('nav').exists()).toBe(true)
  })

  it('restores the session token after a reload without asking again', async () => {
    sessionStorage.setItem(SESSION_TOKEN_KEY, sessionToken)
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.find('input[type="password"]').exists()).toBe(false)
    expect(fetch).toHaveBeenCalledWith('/api/v1/monitoring', expect.anything())
  })

  it('says the token was rejected, drops it and shows only the login on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({ error: { message: 'unauthorized' } }, 401))))
    sessionStorage.setItem(SESSION_TOKEN_KEY, 'wrong-token')
    const wrapper = mount(App)
    await flushPromises()

    expect(sessionStorage.getItem(SESSION_TOKEN_KEY)).toBeNull()
    expect(wrapper.get('[role="alert"]').text()).toContain('Management token rejected')
    expect(wrapper.text()).not.toContain('No management token')
    expect(wrapper.findAll('input[type="password"]')).toHaveLength(1)
    expect(wrapper.find('nav').exists()).toBe(false)
  })

  it('signs out: forgets the token, stops requests and asks again without blaming the token', async () => {
    sessionStorage.setItem(SESSION_TOKEN_KEY, sessionToken)
    const wrapper = mount(App)
    await flushPromises()
    const before = vi.mocked(fetch).mock.calls.length

    const signOut = wrapper.findAll('header button').find((button) => button.text() === 'Sign out')!
    await signOut.trigger('click')
    await flushPromises()

    expect(sessionStorage.getItem(SESSION_TOKEN_KEY)).toBeNull()
    expect(wrapper.get('[role="status"]').text()).toContain('No management token')
    expect(wrapper.find('[role="alert"]').exists()).toBe(false)
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(vi.mocked(fetch).mock.calls.length).toBe(before)
  })

  describe('configuration draft', () => {
    const configFetch = () => vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/config/validate') return Promise.resolve(jsonResponse({ error: { message: 'expired' } }, 401))
      if (url === '/api/v1/config') return Promise.resolve(jsonResponse(emptyConfigSnapshot()))
      return Promise.resolve(jsonResponse(contractMonitoring()))
    })
    const version = (wrapper: ReturnType<typeof mount>) =>
      (wrapper.get('[data-field="version"] input').element as HTMLInputElement).value
    const login = async (wrapper: ReturnType<typeof mount>) => {
      await wrapper.get('#management-access-token').setValue('second-token')
      await wrapper.get('form').trigger('submit')
      await flushPromises()
    }
    const validate = async (wrapper: ReturnType<typeof mount>) => {
      await wrapper.get('[data-field="version"] input').setValue(2)
      await wrapper.findAll('button').find((button) => button.text().includes('Validate candidate'))!.trigger('click')
      await flushPromises()
    }

    it('survives a rejected token and a new login', async () => {
      window.location.hash = '#/configuration'
      sessionStorage.setItem(SESSION_TOKEN_KEY, sessionToken)
      vi.stubGlobal('fetch', configFetch())
      const wrapper = mount(App)
      await flushPromises()
      await validate(wrapper)

      expect(wrapper.get('[role="alert"]').text()).toContain('Management token rejected')
      await login(wrapper)
      expect(version(wrapper)).toBe('2')
    })

    it('is discarded when the operator signs out', async () => {
      window.location.hash = '#/configuration'
      sessionStorage.setItem(SESSION_TOKEN_KEY, sessionToken)
      vi.stubGlobal('fetch', configFetch())
      const wrapper = mount(App)
      await flushPromises()
      await wrapper.get('[data-field="version"] input').setValue(2)
      await wrapper.findAll('header button').find((button) => button.text() === 'Sign out')!.trigger('click')
      await login(wrapper)
      expect(version(wrapper)).toBe('1')
    })
  })
})
