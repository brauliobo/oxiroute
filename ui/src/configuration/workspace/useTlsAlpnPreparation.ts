import { ref, type Ref } from 'vue'

import type { CanonicalConfig, CertificateConfig } from '../../config'
import { prepareTlsAlpnDeployment } from '../tlsAlpnDeployment'

export interface TlsAlpnMessage {
  kind: 'success' | 'error'
  title: string
  detail: string
}

interface TlsAlpnPreparationOptions {
  draft: Ref<CanonicalConfig | null>
  selectedCertificate: Ref<CertificateConfig | null>
  selectedKey: Ref<string>
  markDraftChanged: () => void
}

export function useTlsAlpnPreparation(options: TlsAlpnPreparationOptions) {
  const tlsAlpnMessage = ref<TlsAlpnMessage | null>(null)

  function prepareTlsAlpnListener(): void {
    const config = options.draft.value
    const certificate = options.selectedCertificate.value
    if (!config || !certificate) return

    const result = prepareTlsAlpnDeployment(config, certificate.name)
    if (result.outcome === 'blocked') {
      tlsAlpnMessage.value = {
        kind: 'error',
        title: 'TLS-ALPN listener not prepared.',
        detail: result.message,
      }
      return
    }
    if (result.outcome === 'ready') {
      tlsAlpnMessage.value = {
        kind: 'success',
        title: 'TLS-ALPN listener is ready in the draft.',
        detail: `${result.listenerName} uses ${result.profileName} on ${result.bindAddress}. Validate and save the certificate challenge selection; public DNS, firewall, and external reachability remain deployment gates.`,
      }
      return
    }

    options.markDraftChanged()
    options.selectedKey.value = `listeners:${result.listenerIndex}`
    tlsAlpnMessage.value = {
      kind: 'success',
      title: 'TLS-ALPN listener draft prepared.',
      detail: `${result.listenerName} will bind ${result.bindAddress} with ${result.profileName} and return 404 for non-challenge HTTP traffic. Nothing is deployed until validation, review, and the revision-checked save.`,
    }
  }

  return { tlsAlpnMessage, prepareTlsAlpnListener }
}
