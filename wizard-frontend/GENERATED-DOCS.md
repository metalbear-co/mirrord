# Mirrord Wizard Frontend Documentation

## Overview

The Mirrord Wizard Frontend is a React-based web application that provides a user-friendly interface for creating and managing mirrord configuration files. The application guides users through the process of setting up mirrord configurations for Kubernetes development environments.

## Project Structure

```
wizard-frontend/
├── src/
│   ├── components/           # React components
│   ├── pages/               # Page components
│   ├── types/               # TypeScript type definitions
│   ├── hooks/               # Custom React hooks
│   ├── lib/                 # Utility functions
│   ├── assets/              # Static assets
│   └── main.tsx             # Application entry point
├── public/                  # Public static files
├── dist/                    # Built application
└── package.json             # Dependencies and scripts
```

## Technology Stack

- **React 18.3.1** - Frontend framework
- **TypeScript 5.5.3** - Type safety
- **Vite 5.4.1** - Build tool and dev server
- **Tailwind CSS 3.4.11** - Styling framework
- **Radix UI** - Accessible component primitives
- **React Router DOM 6.26.2** - Client-side routing
- **TanStack Query 5.56.2** - Data fetching and caching
- **Lucide React** - Icon library

## Core Components

### App.tsx
The main application component that sets up the routing, providers, and global configuration.

**Key Features:**
- React Query client setup for data management
- Tooltip provider for accessibility
- Toast notifications (both shadcn/ui and Sonner)
- Browser router for client-side navigation
- Route definitions for homepage and 404 pages

### Pages

#### Homepage.tsx
The main landing page that conditionally renders different experiences based on user type.

**Props:**
- `isReturning: boolean` - Determines whether to show new user or returning user experience

**Behavior:**
- Renders `HomepageNewUser` for first-time users
- Renders `HomepageReturning` for returning users

#### NotFound.tsx
404 error page component for handling invalid routes.

### Core Components

#### ConfigWizard.tsx
The main configuration wizard component that handles the complete mirrord configuration process.

**Key Features:**
- Multi-step onboarding flow for new users
- Configuration interface with tabs (Target, Network, Export)
- JSON configuration generation and validation
- Boilerplate configuration selection
- Real-time configuration preview

**Props:**
- `isOpen: boolean` - Controls wizard visibility
- `onClose: () => void` - Callback for closing wizard
- `onSave: (config: ConfigData) => void` - Callback for saving configuration
- `isOverview?: boolean` - Optional overview mode
- `isReturning: boolean` - User type flag

**State Management:**
- Onboarding step tracking (intro, explanation, boilerplate, config)
- Configuration data management
- Tab navigation state
- JSON validation state

#### WizardHeader.tsx
Header component for the configuration wizard that displays step-specific headers and navigation tabs.

**Key Features:**
- Dynamic header content based on onboarding step
- Configuration tabs for Target, Network, and Export
- Boilerplate selection badge display
- Conditional rendering based on user type

**Props:**
- `isReturning: boolean`
- `onboardingStep: OnboardingStep`
- `selectedBoilerplate: string`
- `boilerplateConfigs: BoilerplateCardProps[]`
- `currentTab: string`
- `onTabChange: (value: string) => void`
- `configTarget: string`

#### WizardFooter.tsx
Footer component for the configuration wizard that handles navigation between steps and tabs.

**Key Features:**
- First-time user flow navigation
- Configuration tabs navigation
- Save functionality with validation
- Conditional button states based on form validation

**Props:**
- `isReturning: boolean`
- `onboardingStep: OnboardingStep`
- `currentTab: string`
- `configTarget: string`
- `jsonError: string`
- `editableJson: string`
- Various state setters and handlers

#### StepHeader.tsx
Reusable header component for wizard steps that provides consistent styling and structure.

**Props:**
- `title: string` - Header title
- `description: string` - Header description
- `titleSpecialFormat?: string` - Optional special formatting for title
- `badge?: ReactNode` - Optional badge component

**Features:**
- Consistent DialogHeader structure
- Flexible title formatting
- Optional badge support
- Responsive design

#### BoilerplateCard.tsx
Card component for displaying configuration boilerplate options.

**Props:**
- `id: string` - Unique identifier
- `title: string` - Card title
- `description: string` - Card description
- `features: string[]` - List of features
- `icon: LucideIcon` - Icon component
- `color: string` - Color theme
- `selected: boolean` - Selection state
- `onClick: () => void` - Click handler

**Features:**
- Visual selection state
- Feature badges display
- Icon integration
- Hover effects and transitions

#### Panel.tsx
Panel component used on the homepage for displaying action cards.

**Props:**
- `title: ReactNode` - Panel title
- `content: string` - Panel description
- `buttonText: string` - Button label
- `buttonColor: "purple" | "gray"` - Button color theme
- `steps: WizardStep[]` - Wizard steps (currently unused)
- `isReturning: boolean` - User type flag

**Features:**
- Gradient card styling
- Conditional button styling
- Wizard integration
- Responsive design

#### DownloadButton.tsx
Button component for downloading configuration JSON files.

**Props:**
- `json: string` - JSON content to download
- `filename?: string` - Optional filename (defaults to "mirrord-config")

**Features:**
- Blob creation for file download
- Toast notification on download
- Automatic filename generation
- Clean URL management

#### JsonUtils.tsx
Utility functions for JSON configuration handling.

**Functions:**
- `generateConfigJson(config: ConfigData): string` - Converts configuration data to JSON format
- `validateJson(jsonString: string, setJsonError: (error: string) => void): boolean` - Validates JSON syntax
- `updateConfigFromJson(jsonString: string, setConfig: React.Dispatch<React.SetStateAction<ConfigData>>, setJsonError: (error: string) => void): void` - Updates configuration from JSON

**Features:**
- Type-safe configuration transformation
- Error handling and validation
- State management integration

#### Wizard.tsx
Generic wizard component for multi-step processes.

**Props:**
- `steps: WizardStep[]` - Array of wizard steps
- `onComplete?: (data: any) => void` - Completion callback
- `className?: string` - Optional CSS classes
- `data?: any` - Initial data
- `onDataChange?: (data: any) => void` - Data change callback

**Features:**
- Step navigation with back/next buttons
- Data persistence across steps
- Progress indication
- Flexible content rendering

### Homepage Components

#### HomepageNewUser.tsx
Homepage component for first-time users with onboarding-focused content.

**Features:**
- Welcome message and branding
- Two main action panels (Learn and Create Configuration)
- Mirrord logo and version display
- Responsive design for mobile and desktop

#### HomepageReturning.tsx
Homepage component for returning users with streamlined navigation.

**Features:**
- Welcome back message
- Quick access to configuration creation
- Overview and learning options
- Consistent panel layout

## Type Definitions

### types/config.ts

**Note: This file contains unused code and interfaces that are not and should not be implemented! They are here to eventually be resolved into a config type.**

#### FeatureConfig
Defines the structure for mirrord feature configurations including network, filesystem, and environment settings.

#### ConfigData
Main configuration interface used throughout the application for managing mirrord settings.

**Key Properties:**
- `name: string` - Configuration name
- `target: string` - Kubernetes target
- `targetType: string` - Resource type (deployment, pod, etc.)
- `namespace: string` - Kubernetes namespace
- `fileSystem` - Filesystem configuration options
- `network` - Network configuration (incoming/outgoing)
- `environment` - Environment variable settings
- `agent` - Agent-specific settings
- `isActive: boolean` - Active configuration flag

#### Config (Unused) and ConfigPrime (Unused)
More unused configuration interfaces with similar structure to Config.

## Hooks

### use-mobile.tsx
Custom hook for detecting mobile device viewport.

### use-toast.ts
Toast notification hook for displaying user feedback messages.

## Utilities

### lib/utils.ts
Utility functions including the `cn` function for conditional CSS class merging using clsx and tailwind-merge.

## Assets

### mirrord-architecture.svg
Architecture diagram showing how mirrord works.

### mirrord-logo.png
Mirrord brand logo used in the application header.

## Build and Development

### Scripts
- `npm run dev` - Start development server
- `npm run build` - Build production bundle
- `npm run build:dev` - Build development bundle
- `npm run lint` - Run ESLint
- `npm run preview` - Preview production build

### Configuration Files
- `vite.config.ts` - Vite build configuration
- `tailwind.config.ts` - Tailwind CSS configuration
- `tsconfig.json` - TypeScript configuration
- `eslint.config.js` - ESLint configuration
- `postcss.config.js` - PostCSS configuration

## Architecture Patterns

### Component Composition
The application uses a composition pattern where complex components are broken down into smaller, reusable pieces (e.g., WizardHeader, WizardFooter, StepHeader).

### State Management
Local state management using React hooks with prop drilling for complex state sharing between components.

### Type Safety
Comprehensive TypeScript usage with strict typing for all components, props, and data structures.

### Responsive Design
Mobile-first approach using Tailwind CSS with responsive breakpoints and flexible layouts.

## User Flow

### New User Experience
1. Welcome page with mirrord introduction
2. Option to learn about mirrord or skip to configuration
3. Onboarding wizard with explanation steps
4. Boilerplate configuration selection
5. Detailed configuration setup
6. JSON export and download

### Returning User Experience
1. Streamlined welcome back page
2. Direct access to configuration creation
3. Quick access to learning resources
4. Simplified wizard flow without boilerplate selection

## Future Considerations

### Unused Code
The `types/config.ts` file contains interfaces (Config, ConfigPrime) that are not currently used in the application. These may be remnants from previous implementations or planned features.

### TODO Items
Several TODO comments throughout the codebase indicate planned features:
- Backend integration for target fetching
- Enhanced wizard step management
- Improved error handling
- Additional configuration options <- ????
- clean up all the unused imports
- remove unused components and ui stuff

### Scalability
The current architecture supports easy addition of new configuration options and wizard steps through the modular component design.
