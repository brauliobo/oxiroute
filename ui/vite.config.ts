import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The built UI must run under this policy: `vite preview` serves it for the browser tests, which fail on any
// violation (tests/browser/csp.spec.ts).
const strictContentSecurityPolicy = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "connect-src 'self'",
  "img-src 'self'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join('; ')

export default defineConfig({
  plugins: [vue()],
  preview: { headers: { 'Content-Security-Policy': strictContentSecurityPolicy } },
})
