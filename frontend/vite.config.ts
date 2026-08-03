import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import Components from 'unplugin-vue-components/vite'
import { ElementPlusResolver } from 'unplugin-vue-components/resolvers'
import path from 'path'

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    vue(),
    // On-demand Element Plus: resolves <el-*> components and v-loading
    // directives in templates, injecting per-component JS + CSS at build time
    // instead of bundling the whole library.
    Components({
      dts: true,
      directives: true,
      resolvers: [ElementPlusResolver({ importStyle: 'css', directives: true })],
    }),
  ],
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:18080',
        changeOrigin: true,
      },
    },
  },
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  build: {
    rolldownOptions: {
      output: {
        // Vendor code splitting (rolldown `codeSplitting.groups`; the
        // deprecated `manualChunks` merges sibling groups in this version).
        // `priority` > 0 groups claim their modules first; `vendor-misc` catches
        // any remaining node_modules. Each group becomes an independently
        // cacheable chunk instead of one 1.4 MB bundle.
        codeSplitting: {
          groups: [
            // Vue core + router + state (used by every route)
            {
              name: 'vendor-vue',
              test: /node_modules[\\/](@vue|vue|vue-router|pinia|@vue[\\/]devtools-api)[\\/]/,
              priority: 20,
            },
            // Element Plus on-demand subset (component core). `maxSize` forces
            // rolldown to split this group into sub-chunks (its sole-importer
            // deps fold into it), keeping every emitted chunk < 500 kB.
            {
              name: 'vendor-element-plus',
              test: /node_modules[\\/]element-plus[\\/]/,
              priority: 20,
              maxSize: 480 * 1024,
            },
            // Element Plus runtime deps + icons (standalone packages)
            {
              name: 'vendor-element-plus-deps',
              test: /node_modules[\\/](@element-plus|@vueuse|@floating-ui|@popperjs|dayjs|lodash-es|async-validator|@ctrl|normalize-wheel-es)[\\/]/,
              priority: 20,
            },
            // Heavy chart lib + its dep, only used by Dashboard route
            {
              name: 'vendor-charts',
              test: /node_modules[\\/](lightweight-charts|fancy-canvas)[\\/]/,
              priority: 20,
            },
            // Any other third-party package
            {
              name: 'vendor-misc',
              test: /node_modules/,
              priority: 10,
            },
          ],
        },
      },
    },
  },
})
