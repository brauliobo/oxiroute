import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { consoleContentSecurityPolicy, designAlias } from './vite.shared'

export default defineConfig({
  plugins: [vue()],
  resolve: { alias: designAlias },
  preview: { headers: { 'Content-Security-Policy': consoleContentSecurityPolicy } },
})
