import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'path'

// https://vite.dev/config/
export default defineConfig({
  plugins: [vue()],
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
        // Rollup-compatible manual chunking (rolldown-vite maps this onto
        // output.codeSplitting groups). Keeps big third-party vendors in
        // separate, cacheable chunks instead of one 1.4 MB bundle.
        manualChunks(id) {
          if (!id.includes('node_modules')) return undefined

          // Vue core + router + state (used by every route)
          if (/[\\/]node_modules[\\/](@vue|vue|vue-router|pinia|@vue\/devtools-api)[\\/]/.test(id)) {
            return 'vendor-vue'
          }

          // Element Plus + its runtime deps (icons, vueuse, popper, etc.)
          if (
            /[\\/]node_modules[\\/](element-plus|@element-plus|@vueuse|@floating-ui|@popperjs|dayjs|lodash-es|async-validator|@ctrl|normalize-wheel-es)[\\/]/.test(
              id,
            )
          ) {
            return 'vendor-element-plus'
          }

          // Heavy chart lib, only used by Dashboard route
          if (/[\\/]node_modules[\\/](lightweight-charts)[\\/]/.test(id)) {
            return 'vendor-charts'
          }

          // HTTP client
          if (/[\\/]node_modules[\\/](axios)[\\/]/.test(id)) {
            return 'vendor-http'
          }

          // Any other third-party package
          return 'vendor-misc'
        },
      },
    },
  },
})
