import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://localhost:59281',
      '/ws': {
        target: 'ws://localhost:59281',
        ws: true,
      },
    },
  },
})
