import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api/topology': {
        target: 'https://localhost:9443',
        secure: false,
        rewrite: (path: string) => path.replace('/api/topology', '/topology'),
      },
      '/api': 'http://localhost:59293',
      '/ws': {
        target: 'ws://localhost:59293',
        ws: true,
      },
    },
  },
})
