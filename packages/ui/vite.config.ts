import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import fs from 'fs'
import path from 'path'

// The bundle is embedded into the CLI binary built from the same checkout, so the workspace
// version is exactly the mirrord version this UI ships in. Telemetry stamps it on every event.
const cargoToml = fs.readFileSync(
  path.resolve(__dirname, '../../Cargo.toml'),
  'utf8',
)
const mirrordVersion = /^version = "([^"]+)"/m.exec(
  cargoToml.slice(cargoToml.indexOf('[workspace.package]')),
)?.[1]
if (!mirrordVersion) {
  throw new Error('failed to read the workspace version from Cargo.toml')
}

// `mirrord-ui` is the single built site. It composes two feature packages — the session monitor
// (`packages/monitor`) and the config wizard (`packages/wizard`) — which are compiled straight from
// source via the aliases below. The shell in `src/` lazy-loads whichever one the current route
// needs, so each feature's (conflicting) CSS tokens only ever load on its own page.
export default defineConfig({
  plugins: [react()],
  define: {
    __MIRRORD_VERSION__: JSON.stringify(mirrordVersion),
  },
  resolve: {
    alias: {
      // Order matters: the more specific `/theme` subpath must precede the package root so it is
      // not shadowed. The shell imports just the theme module (no monitor component graph) to
      // apply the shared light/dark preference on both routes.
      '@mirrord/monitor/theme': path.resolve(
        __dirname,
        '../monitor/src/theme.ts',
      ),
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
