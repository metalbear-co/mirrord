# mirrord Configuration Wizard Frontend

A modern React-based web application for creating and managing mirrord configuration files. This wizard provides an intuitive interface for developers to configure mirrord without manually editing JSON files.

## 🚀 Features

- **Interactive Configuration Wizard**: Step-by-step guided setup for mirrord configurations
- **Multiple Configuration Modes**: Support for steal, mirror, and replace modes
- **Visual Configuration Management**: Manage multiple configurations with an intuitive dashboard
- **Real-time JSON Generation**: Live preview and editing of generated configuration files
- **Responsive Design**: Works seamlessly on desktop and mobile devices
- **Modern UI Components**: Built with shadcn/ui and Tailwind CSS

## 🛠️ Tech Stack

- **Frontend Framework**: React 18 with TypeScript
- **Build Tool**: Vite
- **Styling**: Tailwind CSS with custom design system
- **UI Components**: shadcn/ui (Radix UI primitives)
- **State Management**: React hooks and localStorage
- **Routing**: React Router DOM
- **Icons**: Lucide React
- **Form Handling**: React Hook Form with Zod validation

## 📁 Project Structure

```
wizard-frontend/
├── src/
│   ├── components/           # Reusable UI components
│   │   ├── ui/              # shadcn/ui components
│   │   ├── config/          # Configuration-specific components
│   │   ├── AppSidebar.tsx   # Main navigation sidebar
│   │   ├── ConfigWizard.tsx # Main configuration wizard
│   │   ├── Dashboard.tsx    # Main dashboard component
│   │   └── Header.tsx       # Application header
│   ├── pages/               # Route components
│   │   ├── Index.tsx        # Landing page with onboarding
│   │   ├── Dashboard.tsx    # Main dashboard page
│   │   ├── Onboarding.tsx   # User onboarding flow
│   │   └── StyleGuide.tsx   # Component style guide
│   ├── types/               # TypeScript type definitions
│   │   └── config.ts        # Configuration data types
│   ├── hooks/               # Custom React hooks
│   ├── lib/                 # Utility functions
│   └── assets/              # Static assets
├── public/                  # Public static files
├── dist/                    # Built application
└── configuration files      # Build and tooling configs
```

## 🚀 Getting Started

### Prerequisites

- Node.js 18+ 
- npm or yarn

### Installation

1. **Clone the repository**
   ```bash
   git clone <repository-url>
   cd mirrord/wizard-frontend
   ```

2. **Install dependencies**
   ```bash
   npm install
   ```

3. **Start development server**
   ```bash
   npm run dev
   ```

4. **Open your browser**
   Navigate to `http://localhost:8080`

### Available Scripts

- `npm run dev` - Start development server
- `npm run build` - Build for production
- `npm run build:dev` - Build for development
- `npm run preview` - Preview production build
- `npm run lint` - Run ESLint

## 🎯 Core Components

### ConfigWizard

The main configuration wizard component that guides users through creating mirrord configurations.

**Key Features:**
- Multi-step configuration process
- Target selection (Kubernetes resources)
- Network configuration (incoming/outgoing traffic)
- File system and environment settings
- Real-time JSON generation and validation

**Props:**
```typescript
interface ConfigWizardProps {
  isOpen: boolean;
  onClose: () => void;
  onSave: (config: ConfigData) => void;
  existingConfigs?: ConfigData[];
  mode?: 'create' | 'overview';
}
```

### Dashboard

The main dashboard for managing configurations and navigating the application.

**Features:**
- Configuration overview
- Service grouping
- Active configuration management
- Quick access to wizard

### Onboarding

Interactive onboarding flow for new users.

**Steps:**
1. Welcome screen
2. mirrord introduction
3. Architecture explanation
4. Configuration modes (steal, mirror, replace)
5. Development feedback loop
6. Configuration creation

## 🔧 Configuration Types

The application supports comprehensive mirrord configuration options:

### Network Configuration
- **Incoming Traffic**: Steal or mirror modes
- **HTTP Filtering**: Header and path-based filtering
- **Port Mapping**: Local to remote port mappings
- **Outgoing Traffic**: Protocol and target filtering
- **DNS**: DNS resolution configuration

### File System
- **Modes**: Read, write, or local
- **Rules**: Custom file system access rules

### Environment
- **Include/Exclude**: Environment variable filtering
- **Override**: Custom environment variable values

### Agent Settings
- **Scale Down**: Scale down target resources
- **Copy Target**: Copy target configuration

## 🎨 Design System

The application uses a custom design system built on Tailwind CSS:

### Colors
- **Primary**: Purple gradient (`hsl(258 90% 66%)` to `hsl(272 91% 70%)`)
- **Background**: Light/dark theme support
- **Semantic Colors**: Success, warning, error states

### Components
- **Cards**: Gradient backgrounds with subtle shadows
- **Buttons**: Multiple variants with hover effects
- **Forms**: Consistent styling with validation states
- **Navigation**: Sidebar with collapsible sections

### Responsive Design
- Mobile-first approach
- Breakpoints: `sm`, `md`, `lg`, `xl`, `2xl`
- Flexible grid layouts
- Touch-friendly interactions

## 📱 User Flows

### New User Flow
1. **Landing Page**: Welcome screen with options
2. **Onboarding**: Learn about mirrord (optional)
3. **Configuration Wizard**: Create first configuration
4. **Dashboard**: Manage configurations

### Returning User Flow
1. **Dashboard**: View existing configurations
2. **Quick Actions**: Create, edit, or duplicate configs
3. **Configuration Management**: Set active, delete, or export

## 🔌 Integration Points

### Local Storage
- Configuration persistence
- User preferences
- Onboarding completion status

### Configuration Export
- JSON file generation
- Clipboard copy functionality
- Download capabilities

### Target Selection
- Mock Kubernetes resource discovery
- Namespace and resource type selection
- Service grouping

## 🧪 Development

### Code Style
- TypeScript strict mode
- ESLint configuration
- Prettier formatting
- Component-based architecture

### State Management
- React hooks for local state
- localStorage for persistence
- Context providers for global state

### Testing
- Component testing setup
- Mock data for development
- Responsive design testing

## 🚀 Deployment

### Build Process
1. TypeScript compilation
2. Vite bundling and optimization
3. Asset processing
4. Static file generation

### Production Build
```bash
npm run build
```

The built application will be in the `dist/` directory, ready for deployment to any static hosting service.

## 📚 API Reference

### Configuration Data Structure

```typescript
interface ConfigData {
  name: string;
  target: string;
  targetType: string;
  namespace: string;
  service?: string;
  fileSystem: {
    enabled: boolean;
    mode: "read" | "write" | "local";
    rules: Array<{
      mode: "read" | "write" | "local";
      filter: string;
    }>;
  };
  network: {
    incoming: {
      enabled: boolean;
      mode: "steal" | "mirror";
      httpFilter: Array<{
        type: "header" | "method" | "content" | "path";
        value: string;
        matchType?: "exact" | "regex";
      }>;
      filterOperator: "AND" | "OR";
      ports: Array<{
        remote: string;
        local: string;
      }>;
    };
    outgoing: {
      enabled: boolean;
      protocol: "tcp" | "udp" | "both";
      filter: string;
      filterTarget: "remote" | "local";
    };
    dns: {
      enabled: boolean;
      filter: string;
    };
  };
  environment: {
    enabled: boolean;
    include: string;
    exclude: string;
    override: string;
  };
  agent: {
    scaledown: boolean;
    copyTarget: boolean;
  };
  isActive: boolean;
}
```

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## 📄 License

This project is part of the mirrord project. See the main repository for license information.

## 🔗 Related Links

- [mirrord Documentation](https://metalbear.co/mirrord/docs)
- [mirrord GitHub](https://github.com/metalbear-co/mirrord)
- [shadcn/ui Components](https://ui.shadcn.com/)
- [Tailwind CSS](https://tailwindcss.com/)