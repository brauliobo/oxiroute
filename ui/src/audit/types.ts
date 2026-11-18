import type { AuditCategory, AuditResult } from '../api'

export interface AuditFilterSelection {
  category: AuditCategory | ''
  result: AuditResult | ''
}
