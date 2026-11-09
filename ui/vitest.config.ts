import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { defineConfig, mergeConfig } from 'vitest/config'
import viteConfig from './vite.config'

// Vue's Vapor runtime only ships in the esm-bundler builds, but Node's "node" export condition resolves `vue` and the
// `@vue/*` runtime packages to their CommonJS builds, which have none. Alias them all to the bundler builds that the
// production build uses; the packages are not direct dependencies, so locate them from `vue` (strict pnpm layout).
const packageDir = (name: string, from: string) => dirname(createRequire(join(from, 'package.json')).resolve(`${name}/package.json`))
const vueDir = packageDir('vue', import.meta.dirname)
const runtimeDomDir = packageDir('@vue/runtime-dom', vueDir)
const runtimeCoreDir = packageDir('@vue/runtime-core', runtimeDomDir)
const runtimePackages = {
  'runtime-dom': runtimeDomDir,
  'runtime-core': runtimeCoreDir,
  'runtime-vapor': packageDir('@vue/runtime-vapor', vueDir),
  reactivity: packageDir('@vue/reactivity', runtimeCoreDir),
  shared: packageDir('@vue/shared', runtimeCoreDir),
}

export default mergeConfig(viteConfig, defineConfig({
  resolve: {
    alias: [
      { find: /^vue$/, replacement: join(vueDir, 'dist/vue.runtime.esm-bundler.js') },
      ...Object.entries(runtimePackages).map(([name, dir]) => ({
        find: new RegExp(`^@vue/${name}$`),
        replacement: join(dir, `dist/${name}.esm-bundler.js`),
      })),
    ],
  },
  test: {
    environment: 'jsdom',
    exclude: ['**/node_modules/**', 'dist/**', 'tests/**'],
    // Inlined so the DOMWrapper it provides imports `vue` through the aliases above (a single Vue instance).
    server: { deps: { inline: [/@vue\/test-utils/] } },
  },
}))
