import App from './App'

// Root export for the config wizard, consumed by the `mirrord-ui` shell (see `packages/ui`). The
// shared `QueryClientProvider`, the theme, and all global CSS now live in the shell, so this just
// renders the wizard app.
export default function Wizard() {
  return <App />
}
