# mirrord Wizard Frontend - Business Logic

## Overview

The wizard frontend is a React application that helps users create a `mirrord-up.yaml`, which
they run with `mirrord up -f <file>`. It asks the same questions as `mirrord up init`, in the same
order and with the same wording, and the `mirrord ui` server turns what the user picked into the
YAML with the code `mirrord up init` uses (`mirrord_up::render_skeleton`). So the same picks give
the same file. `mirrord wizard --tui` runs `mirrord up init` itself.

## Application Flow

### 1. Entry Point (App.tsx → Homepage.tsx)

The homepage offers "Get Started" (straight to the questions) and "Learn the basics first" (the
learning pages, then the questions).

### 2. Questions for each service

The steps follow `mirrord up init` (`mirrord/up/src/init.rs`):

1. **Mode** (`ModeStep`): Split (recommended, preselected), Mirror or Replace. The modes, their
   labels and descriptions come from `GET /api/v1/up/modes`.
2. **Target** (`TargetStep`): a namespace and a workload, from the kube context `mirrord up init`
   and `mirrord up` use: the `kube_context` of the global config, or the kubeconfig's current
   context. It is shown but cannot be changed (`mirrord up init` has no context question). The
   workloads come from `GET /api/v1/up/workloads`, which lists them with the code of
   `mirrord up init` (only the kinds the mode allows, one entry per container of a pod with
   several). Split also offers "Run without a target (outgoing traffic only)". The service is
   named after the workload (`<workload>-2` and so on when the name is taken); a targetless
   service is named by the user.
3. **Traffic** (`TrafficStep`), skipped for a targetless service: the mode's `traffic_question`,
   off by default. When off: the mode's `traffic_default`, what `mirrord up` does without
   customizing, linking to the service modes docs. When on: the HTTP header filter (not in
   Replace), the ports the target declares (from `GET /api/v1/up/ports`) to intercept (all
   ticked; unticked ones are ignored), and other ports to ignore.
4. **Run** (`RunStep`): the local command (required) and the working directory.
5. **Export** (`ExportStep`): the services so far, "Add another service" (back to Mode), the YAML
   from `POST /api/v1/up/render`, copy and download as `mirrord-up.yaml`, and how to run it. It
   also points to the `common` block for the settings every service shares, which `mirrord up init`
   doesn't ask about because each has a good default.

Going back from the Mode step of another service drops it and returns to Export.

## API Routes

```typescript
const ALL_API_ROUTES = {
  isReturning: '/api/v1/is-returning',
  upModes: '/api/v1/up/modes', // { modes: ModeInfo[] }
  upWorkloads: (namespace, mode) => '/api/v1/up/workloads?namespace=...&mode=...', // { workloads: string[] }
  upPorts: (namespace, path) => '/api/v1/up/ports?namespace=...&path=...', // { ports: number[] }
  upRender: '/api/v1/up/render', // POST { services: PickedService[] } → { yaml } or 400 { error }
  contexts: '/api/v2/kube/contexts',
  namespaces: '/api/v2/kube/namespaces',
}
// Errors come back as { error: string }, which the wizard shows.
```

## Picked services (lib/up.ts)

```typescript
interface PickedService {
  name: string
  mode: 'split' | 'mirror' | 'replace'
  target: { path: string; namespace: string } | null // null: targetless
  header_filter: string | null // null: the default session-key filter
  ignore_ports: number[]
  command: string // split on whitespace by the server
  directory: string | null
}
```

The service being edited is a `ServiceDraft`. `toPickedService` turns it into `PickedService`: without
the traffic question answered, there is no header filter and no ignored port.

## State Management

`DraftContext` holds the finished services and the service being edited. Closing the wizard
drops both.
