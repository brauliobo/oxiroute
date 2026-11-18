import { vi } from 'vitest'
import { jsonResponse } from './contractFixtures'
import { managementGeneration, managementListeners, managementStatus } from './managementFixtures'
import { inflightList, operationsInventory, registrationList } from './operationsFixtures'

export interface RecordedCall {
  url: string
  method: string
  body: unknown
}

type Reply = (call: RecordedCall) => Response | undefined

export function operationsFetch(reply: Reply = () => undefined) {
  const calls: RecordedCall[] = []
  const inventory = operationsInventory()
  const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), method: init?.method ?? 'GET', body: init?.body ? JSON.parse(String(init.body)) : null }
    calls.push(call)
    return Promise.resolve(reply(call) ?? defaultReply(call, inventory))
  })
  const mutations = () => calls.filter((call) => call.method !== 'GET')
  return { fetch, calls, mutations }
}

function defaultReply(call: RecordedCall, inventory: ReturnType<typeof operationsInventory>): Response {
  const path = call.url.split('?')[0]
  if (call.method !== 'GET') return jsonResponse({ outcome: 'applied', changed: 1 })
  if (path === '/api/v1/status') return jsonResponse(managementStatus())
  if (path === '/api/v1/generations') return jsonResponse(managementGeneration())
  if (path === '/api/v1/listeners') return jsonResponse(managementListeners())
  if (path === '/api/v1/pools') return jsonResponse(inventory.pools)
  if (path === '/api/v1/servers') return jsonResponse(inventory.servers)
  if (path === '/api/v1/requests') return jsonResponse(inflightList())
  if (path === '/api/v1/registrations') return jsonResponse(registrationList())
  throw new Error(`Unexpected request: ${call.method} ${call.url}`)
}
