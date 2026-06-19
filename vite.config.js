import legacy from '@vitejs/plugin-legacy'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [
    vue(),
    legacy({
      targets: ['Chrome >= 69', 'Firefox >= 59'],
    }),
  ],
  build: {
    outDir: 'frontend-dist',
    emptyOutDir: true,
  },
  test: {
    environment: 'jsdom',
  },
})
