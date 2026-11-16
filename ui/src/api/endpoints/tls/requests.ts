import { request } from '../../transport'
import { authorizationHeader, mutationHeaders } from '../shared'
import { parseTlsActionResponse, parseTlsInventory, parseTlsReconcileResponse, parseTlsRenewResponse } from './decode'
import type { TlsActionResponse, TlsInventory, TlsReconcileResponse, TlsRenewResponse } from './types'

export async function fetchTlsInventory(token: string, signal?: AbortSignal): Promise<TlsInventory> {
  return parseTlsInventory(await request('/api/v1/tls', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function reconcileTls(
  expectedActiveRevision: string, token: string, certificate?: string,
): Promise<TlsReconcileResponse> {
  return parseTlsReconcileResponse(await request('/api/v1/tls/reconcile', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, ...(certificate ? { certificate } : {}) }),
  }))
}

export async function renewManagedCertificate(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsRenewResponse> {
  return parseTlsRenewResponse(await request('/api/v1/tls/renew', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}

export async function revokeManagedCertificate(
  expectedActiveRevision: string,
  token: string,
  certificate: string,
  reason?: number,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/revoke', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate, ...(reason === undefined ? {} : { reason }) }),
  }))
}

export async function deleteManagedCertificate(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/delete', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}

export async function rolloverManagedAccountKey(
  expectedActiveRevision: string,
  token: string,
  certificate?: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/account/rollover', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, ...(certificate ? { certificate } : {}) }),
  }))
}

export async function cancelManagedJob(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/cancel', expectedActiveRevision, token, certificate)
}

export async function pauseManagedJobs(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/pause', expectedActiveRevision, token, certificate)
}

export async function resumeManagedJobs(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/resume', expectedActiveRevision, token, certificate)
}

async function postTlsAction(
  url: string, expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request(url, {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}
