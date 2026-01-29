# mirrord Wizard Frontend - Business Logic

## Overview

The wizard frontend is a React application that helps users create `mirrord.json` configuration files through a guided wizard interface. It connects to a backend API to fetch Kubernetes cluster information.

## Application Flow

### 1. Entry Point (App.tsx → Homepage.tsx)

1. App checks if user is "returning" via API call to `/api/v1/is-returning`
2. Based on response, shows either:
   - **New User**: `HomepageNewUser` - Welcome message + two panels (Learn First / Skip to Config)
   - **Returning User**: `HomepageReturning` - Welcome back + two panels (Create Config / Learn)

### 2. Homepage Panels

Each homepage has two clickable panels that open different wizard flows:

**New User:**
- "Learn About mirrord First" → IntroStep + 6 Learning Steps + BoilerplateStep + ConfigStep
- "Skip to Configuration" → BoilerplateStep + ConfigStep

**Returning User:**
- "Create Configuration" → BoilerplateStep + ConfigStep
- "Learn About mirrord" → 6 Learning Steps + BoilerplateStep + ConfigStep

### 3. Wizard Steps

#### Step: BoilerplateStep
User selects one of three modes:
- **Filtering Mode** (steal): Selective traffic stealing with HTTP header/path filters
- **Mirror Mode**: Copy traffic without disrupting remote service
- **Replace Mode**: Completely substitute remote service (copy_target + scale_down)

#### Step: ConfigStep (ConfigTabs)
Three tabs:
1. **Target Tab**: Select namespace, target type, and specific target from K8s cluster
2. **Network Tab**: Configure incoming traffic, filters, and port mappings
3. **Export Tab**: View JSON, copy to clipboard, or download file

## API Routes

```typescript
const ALL_API_ROUTES = {
  isReturning: "/api/v1/is-returning",
  clusterDetails: "/api/v1/cluster-details",  // Returns { namespaces: string[], target_types: string[] }
  targets: (namespace: string, targetType?: string) =>
    "/api/v1/namespace/{namespace}/targets?target_type={targetType}"
    // Returns Target[] where Target = { target_path, target_namespace, detected_ports }
};
```

## Config Data Structure

### Default Config
```typescript
const DefaultConfig: LayerFileConfig = {
  feature: {
    network: {
      incoming: { mode: "mirror" },
      outgoing: true,
    },
    fs: "read",
    env: true,
  },
};
```

### Key Config Sections

1. **target**: `{ path: "deployment/name", namespace: "default" }`
2. **feature.network.incoming.mode**: `"mirror"` | `"steal"`
3. **feature.network.incoming.ports**: `number[]` - ports to intercept
4. **feature.network.incoming.port_mapping**: `[localPort, remotePort][]`
5. **feature.network.incoming.http_filter**: Header/path filters for selective stealing
6. **feature.copy_target**: `{ enabled: boolean, scale_down: boolean }` - for Replace mode

## Config Manipulation Functions (JsonUtils.tsx)

### Mode/Boilerplate
- `readBoilerplateType(config)` → `"steal"` | `"mirror"` | `"replace"` | `"custom"`
- `updateConfigMode(mode, config)` → Sets `incoming.mode`
- `updateConfigCopyTarget(enabled, scaleDown, config)` → Sets `copy_target`

### Target
- `readCurrentTargetDetails(config)` → `{ type: string, name?: string }`
- `updateConfigTarget(config, targetPath, namespace)` → Sets `target`

### Incoming Network
- `readIncoming(config)` → Returns entire `network.incoming` section
- `updateIncoming(config, newIncoming)` → Replaces `network.incoming`

### HTTP Filters
- `readCurrentFilters(config)` → `{ filters: UiHttpFilter[], operator: "any"|"all"|null }`
- `updateConfigFilter(filters, operator, config)` → Sets `http_filter`
- `removeSingleFilter(filter, config)` → Removes one filter
- `disableConfigFilter(config)` → Clears all filters
- `regexificationRay(value)` → Converts exact match to regex: `"foo"` → `"^foo$"`

Filter types:
```typescript
interface UiHttpFilter {
  value: string;       // The filter pattern (regex)
  type: "header" | "path";
}
```

### Ports
- `readCurrentPorts(config)` → `number[]` - ports from `incoming.ports`
- `updateConfigPorts(ports, config)` → Sets `incoming.ports` (and `http_filter.ports` if filters exist)
- `readCurrentPortMapping(config)` → `[localPort, remotePort][]`
- `updateConfigPortMapping(mappings, config)` → Sets `incoming.port_mapping`
- `addRemoveOrUpdateMapping(remote, local, config)` → Smart mapping update
- `getLocalPort(remotePort, config)` → Returns mapped local port or remote if no mapping
- `removePortandMapping(remotePort, config)` → Removes port and its mapping
- `disablePortsAndMapping(config)` → Clears both ports and mappings

### JSON Export
- `getConfigString(config)` → Pretty-printed JSON (removes `incoming.ports` in replace mode)

## State Management

### UserDataContext
- `UserDataContext`: Stores boolean for `isReturning` status
- `ConfigDataContext`: Stores current `LayerFileConfig` and setter
- `ConfigDataContextProvider`: Wraps app with config state (initialized to `DefaultConfig`)

## UI Components Needed

### Core
- **Wizard**: Dialog with header (title, step counter, mode badge), content area, footer (Back/Next)
- **Panel**: Card with title, description, button → opens wizard

### Form Components (from @metalbear/ui)
- Button, Input, Textarea, Label
- Select (SelectTrigger, SelectContent, SelectItem, SelectValue)
- Tabs (TabsList, TabsTrigger, TabsContent)
- RadioGroup (RadioGroupItem)
- Switch, Checkbox, Separator, Badge
- Dialog (DialogContent, DialogTitle, DialogDescription)
- Toast (ToastProvider, ToastViewport, Toast, ToastTitle, ToastDescription, ToastClose)
- Tooltip (TooltipProvider, Tooltip, TooltipTrigger, TooltipContent)

### Custom (keep local)
- **Card**: Simple card with CardHeader, CardTitle, CardDescription, CardContent, CardFooter
- **Popover**: For target search dropdown (not in @metalbear/ui)

## Assets
- `mirrord-logo.svg` - Logo for homepage
- `mirrord-architecture.svg` - Architecture diagram for learning steps
- `flow-diagram.png` - Feedback loop diagram for learning steps

## Key Business Rules

1. **Replace Mode**: Sets `copy_target.enabled=true`, `copy_target.scale_down=true`, mode="steal"
2. **Mirror Mode**: Sets `copy_target.enabled=false`, mode="mirror"
3. **Filtering Mode**: Sets `copy_target.enabled=false`, mode="steal" (allows filters)
4. **Port Conflicts**: Local ports must be unique across all mappings
5. **Filter Ports**: When filters exist, ports are synced to `http_filter.ports`
6. **Target Required**: Cannot proceed to Network tab without selecting a target
7. **Replace Mode Restrictions**: Cannot delete auto-detected ports, no `incoming.ports` in export
