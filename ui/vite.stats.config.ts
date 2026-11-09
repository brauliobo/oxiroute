import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The stats page: its own entry (stats.html), built to dist-stats/. The page listener serves it under a configured
// prefix, so nothing in the bundle may assume the page is at `/`. Layout and budget: src/stats/README.md.
// `vite preview` serves the build with the policy the listener sends; tests/browser/stats fails on any violation.
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

export default defineConfig({
  plugins: [vue()],
  base: './',
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
