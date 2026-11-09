import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import type { Plugin } from 'vite'

// The stats page: its own entry (stats.html), built to dist-stats/. stats.html is the single source of the shell the
// page listener serves: the build only adds the hashed script and stylesheet links, and scripts/generate-stats-assets.sh
// installs the result as crates/oxiroute-server/assets/stats/index.html. The listener fills in the `/__stats_prefix__`
// token (the configured uri_prefix) and the `__stats_*__` values per request, so the build base is the token itself.
// Layout and budget: src/stats/README.md.
// `vite preview` serves the build with the policy the listener sends and answers the prefix token as the site root
// (the page is at /stats.html); tests/browser/stats fails on any violation.
const PREFIX_TOKEN = '/__stats_prefix__'
const statsContentSecurityPolicy = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "connect-src 'self'",
  "img-src 'self' data:",
  "font-src 'self'",
  "form-action 'self'",
  "base-uri 'none'",
  "frame-ancestors 'none'",
].join('; ')

const prefixTokenIsRoot: Plugin = {
  name: 'stats-prefix-token-is-root',
  configurePreviewServer: ({ middlewares }) => {
    middlewares.use((request, _response, next) => {
      if (request.url?.startsWith(`${PREFIX_TOKEN}/`)) request.url = request.url.slice(PREFIX_TOKEN.length)
      next()
    })
  },
}

export default defineConfig({
  plugins: [vue(), prefixTokenIsRoot],
  base: `${PREFIX_TOKEN}/`,
  build: {
    outDir: 'dist-stats',
    emptyOutDir: true,
    manifest: 'manifest.json',
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: { stats: 'stats.html' },
      output: {
        entryFileNames: 'assets/stats-[hash].js',
        chunkFileNames: 'assets/stats-[name]-[hash].js',
        assetFileNames: 'assets/stats-[hash][extname]',
      },
    },
  },
  preview: { headers: { 'Content-Security-Policy': statsContentSecurityPolicy } },
})
