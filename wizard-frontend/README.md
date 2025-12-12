# mirrord Configuration Wizard Frontend

### Tech Stack

- Core
  - React
  - TypeScript
  - Radix UI (component primitives)
  - TanStack Query (query fetching and caching)
- Design
  - Tailwind CSS
  - Lucide icons
- Dev
  - Vite (Build tool and dev server)

### Directory Layout

```
wizard-frontend/
├── src/
│   ├── assets/              # Static files
│   ├── components/          # Reusable UI components
│   ├── pages/               # Route components
│   ├── hooks/               # Custom React hooks
│   ├── lib/                 # Utility functions
│   └── (other files)        # Entrypoints and config schema
├── public/                  # Public static files
├── dist/                    # Build output
└── (other files)            # Build and tooling configs
```

> NOTE: More documentation can be found in the mirrord CLI crate where the backend code exists, including docs on how to build the backend and frontend together.

## Quick Start

1. Navigate to the source dir from repo root

   ```bash
   cd /wizard-frontend
   ```

2. Install dependencies

   ```bash
   npm install
   ```

3. Start development server

   ```bash
   npm run dev
   ```

   > NOTE: This will start a dev server that is not connected to a backend. The frontend will not be able to list targets or cluster details! In the future, this should support rerouting endpoint calls to a backend running on a different address.

4. Open localhost in the browser: e.g. `http://localhost:8080`

### Helpful Scripts

- `npm run dev`: Start development server
- `npm run build`: Build for production
- `npm run lint`: Run ESLint

## Useful Info

### Important Components

- [`<Homepage>`](./src/pages/Homepage.tsx): Displays the homepage contents depending on if the user has used the wizard before or not.
- [`<Panel>`](./src/components/Panel.tsx): Controls opening a wizard with an array of steps, optionally including learning steps or going straight to config creation.
- [`<Wizard>`](./src/components/Wizard.tsx): Displays `WizardStep`s one at a time, with forward and back buttons, in a `<Dialog>`.
- [`<ConfigTabs>`](./src/components/steps/ConfigTabs.tsx): Contains tabs for 'Target', 'Network' and 'Export' that control config creation after the initial boilerplate config is chosen in [`<BoilerplateStep>`](./src/components/steps/BoilerplateStep.tsx).

### Interaction with Backend (Endpoints)

These are defined in [routes.ts](./src/lib/routes.ts) and imported elsewhere in the code.

- `/api/v1/is-returning`: Called when the app starts to determine layout and copy.
- `/api/v1/cluster-details`: Called when rendering [`<ConfigTabs>`](./src/components/steps/ConfigTabs.tsx) in the 'Target' tab to show user available namespaces and target types.
- `/api/v1/namespace/{namespace}/targets`: Called after the above to show user available targets, including the target's exposed ports (which are used in the 'Config' tab).
  - Accepts query param `?target_type=string` to optionally limit the listed target to a specific type.
