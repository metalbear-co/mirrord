import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'

// `mirrord-ui` is the single built site. It composes two feature packages — the session monitor
// (`packages/monitor`) and the config wizard (`packages/wizard`) — which are compiled straight from
// source via the aliases below. The shell in `src/` lazy-loads whichever one the current route
// needs, so each feature's (conflicting) CSS tokens only ever load on its own page.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      // Order matters: the more specific `/theme` subpath must precede the package root so it is
      // not shadowed. The shell imports just the theme module (no monitor component graph) to
      // apply the shared light/dark preference on both routes.
      '@mirrord/monitor/theme': path.resolve(__dirname, '../monitor/src/theme.ts'),
      '@mirrord/monitor': path.resolve(__dirname, '../monitor/src/index.tsx'),
      '@mirrord/wizard': path.resolve(__dirname, '../wizard/src/index.tsx'),
    },
  },
  server: {
    port: 5173,
    proxy: {
      // Point these at a running `mirrord ui` server (default port 59281).
      '/api': 'http://localhost:59281',
      '/ws': {
        target: 'ws://localhost:59281',
        ws: true,
      },
    },
  },
})
