import { fileURLToPath } from 'node:url'
import type { AliasOptions } from 'vite'

type Directives = Readonly<Record<string, readonly string[]>>

const serialize = (directives: Directives) =>
  Object.entries(directives).map(([name, sources]) => [name, ...sources].join(' ')).join('; ')

const STRICT_DIRECTIVES = {
  'default-src': ["'none'"],
  'script-src': ["'self'"],
  'style-src': ["'self'"],
  'connect-src': ["'self'"],
  'img-src': ["'self'"],
  'base-uri': ["'none'"],
  'form-action': ["'none'"],
  'frame-ancestors': ["'none'"],
} satisfies Directives

export const consoleContentSecurityPolicy = serialize({ ...STRICT_DIRECTIVES, 'media-src': ["'self'"] })

export const statsContentSecurityPolicy = serialize({
  ...STRICT_DIRECTIVES,
  'img-src': ["'self'", 'data:'],
  'font-src': ["'self'"],
  'form-action': ["'self'"],
})

export const designAlias: AliasOptions = {
  '#design': fileURLToPath(new URL('./src/design', import.meta.url)),
}
