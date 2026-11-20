import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import fs from 'fs'
import path from 'path'
import { fileURLToPath } from 'url'

// The directory of this file. Bazel runs Vite with `--configLoader runner`, because Vite's default
// loader bundles this file first, which resolves symbolic links, and in Bazel's sandbox every file
// is a link into Bazel's output tree. The directory would then be outside the sandbox, and
// `packages/monitor` and `packages/wizard` would import second copies of their dependencies from
// there through the aliases below.
const configDirectory = path.dirname(fileURLToPath(import.meta.url))

function workspaceVersion(): string | undefined {
  const cargoToml = fs.readFileSync(
    path.resolve(configDirectory, '../../Cargo.toml'),
    'utf8',
  )
  return /^version = "([^"]+)"/m.exec(
    cargoToml.slice(cargoToml.indexOf('[workspace.package]')),
  )?.[1]
}

// The bundle is embedded into the CLI binary built from the same checkout, so the workspace
// version is exactly the mirrord version this UI ships in. Telemetry stamps it on every event.
//
// Bazel passes the version in `MIRRORD_VERSION` instead, so that the UI's build doesn't depend on
// the root `Cargo.toml` and rerun after every change to it.
const mirrordVersion = process.env.MIRRORD_VERSION || workspaceVersion()
if (!mirrordVersion) {
  throw new Error('failed to read the workspace version from Cargo.toml')
}

const packages = path.resolve(configDirectory, '..')

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
      '@mirrord/monitor/theme': path.resolve(packages, 'monitor/src/theme.ts'),
      '@mirrord/monitor/events': path.resolve(
        packages,
        'monitor/src/events.tsx',
      ),
      '@mirrord/monitor': path.resolve(packages, 'monitor/src/index.tsx'),
      '@mirrord/wizard': path.resolve(packages, 'wizard/src/index.tsx'),
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
