import type { Page, Request } from '@playwright/test'

import { managementGeneration, managementListeners, managementStatus } from '../../../src/test/managementFixtures'
import { inflightList, operationsInventory, registrationList } from '../../../src/test/operationsFixtures'
import { installApiMock, json, managementMonitoring, requestBody, requestPath, type ApiMockResponse } from '../support'

export interface Mutation {
  path: string
  method: string
  body: unknown
}

export type OperationsOverride = (request: Request) => ApiMockResponse | undefined

export async function mockOperations(page: Page, override: OperationsOverride = () => undefined) {
  const mutations: Mutation[] = []
  const reads: string[] = []
  const { pools, servers } = operationsInventory()
  await installApiMock(page, (request) => {
    const path = requestPath(request)
    if (request.method() !== 'GET') {
      mutations.push({ path, method: request.method(), body: requestBody(request) })
      return override(request) ?? json({ outcome: 'applied', changed: 1 })
    }
    reads.push(path)
    const overridden = override(request)
    if (overridden) return overridden
    if (path === '/api/v1/monitoring') return managementMonitoring()
    if (path === '/api/v1/status') return json(managementStatus())
    if (path === '/api/v1/generations') return json(managementGeneration())
    if (path === '/api/v1/listeners') return json(managementListeners())
    if (path === '/api/v1/pools') return json(pools)
    if (path === '/api/v1/servers') return json(servers)
    if (path === '/api/v1/requests') return json(inflightList())
    if (path === '/api/v1/registrations') return json(registrationList())
    return undefined
  })
  return { mutations, reads }
}
