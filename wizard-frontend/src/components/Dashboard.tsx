import { useState } from "react";
import { Settings, Plus, FileCode2, Zap, Save, Edit3, Copy, Trash2, ArrowRight, CheckCircle, BookOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { SidebarProvider, Sidebar, SidebarContent, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarMenu, SidebarMenuItem, SidebarMenuButton, SidebarTrigger } from "@/components/ui/sidebar";
import { ConfigWizard } from "./ConfigWizard";
import mirroredArchitecture from "@/assets/mirrord-architecture.svg";
interface Config {
  id: string;
  name: string;
  target: string;
  targetType: string;
  service: string;
  namespace: string;
  isActive: boolean;
  createdAt: string;
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
}
interface Service {
  name: string;
  configs: Config[];
}
// Temporarily empty to show onboarding flow
const mockServices: Service[] = [];
export function Dashboard() {
  const [showOnboarding, setShowOnboarding] = useState(false);
  const [showWizard, setShowWizard] = useState(false);
  const [showExplanation, setShowExplanation] = useState(false);
  const [services, setServices] = useState<Service[]>(mockServices);
  const [activeSection, setActiveSection] = useState("setup");
  const handleSetActive = (serviceIndex: number, configId: string) => {
    setServices(services.map((service, sIndex) => ({
      ...service,
      configs: service.configs.map(config => ({
        ...config,
        isActive: sIndex === serviceIndex && config.id === configId
      }))
    })));
  };
  const handleDelete = (serviceIndex: number, configId: string) => {
    setServices(services.map((service, sIndex) => {
      if (sIndex === serviceIndex) {
        return {
          ...service,
          configs: service.configs.filter(config => config.id !== configId)
        };
      }
      return service;
    }));
  };
  const handleDuplicate = (serviceIndex: number, configId: string) => {
    const service = services[serviceIndex];
    const configToDupe = service.configs.find(c => c.id === configId);
    if (configToDupe) {
      const newConfig = {
        ...configToDupe,
        id: Date.now().toString(),
        name: `${configToDupe.name} (copy)`,
        isActive: false,
        createdAt: new Date().toISOString().split('T')[0]
      };
      setServices(services.map((s, sIndex) => {
        if (sIndex === serviceIndex) {
          return {
            ...s,
            configs: [...s.configs, newConfig]
          };
        }
        return s;
      }));
    }
  };
  return <SidebarProvider>
      <div className="min-h-screen w-full bg-background">
        <header className="h-14 border-b border-border flex items-center justify-between px-6">
          <div className="flex items-center gap-4">
            <SidebarTrigger />
            <div className="flex items-center gap-2">
              <Zap className="h-6 w-6 text-primary" />
              <span className="text-xl font-bold bg-gradient-primary bg-clip-text text-transparent">
                mirrord
              </span>
            </div>
          </div>
          <Badge variant="secondary" className="text-xs">v3.86.0</Badge>
        </header>

        <div className="flex w-full">
          <Sidebar>
            <SidebarContent>
              <SidebarGroup>
                <SidebarGroupLabel>Dashboard</SidebarGroupLabel>
                <SidebarGroupContent>
                  <SidebarMenu>
                    <SidebarMenuItem>
                      <SidebarMenuButton isActive={activeSection === "setup"} onClick={() => setActiveSection("setup")}>
                        <Settings className="h-4 w-4" />
                        <span>Getting Started</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                    <SidebarMenuItem>
                      <SidebarMenuButton isActive={activeSection === "configs"} onClick={() => setActiveSection("configs")}>
                        <FileCode2 className="h-4 w-4" />
                        <span>Config Files</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            </SidebarContent>
          </Sidebar>

          <main className="flex-1 p-6">
            {activeSection === "setup" && <div className="max-w-4xl">
                <div className="mb-8">
                  <h1 className="text-4xl font-bold mb-4 text-foreground">
                    Getting Started
                  </h1>
                  <p className="text-muted-foreground text-lg max-w-2xl">Run local code like it's in your Kubernetes cluster without deploying it first. Get started by creating your first mirrord.json configuration.</p>
                </div>

            {services.every(service => service.configs.length === 0) ?
            // First-time user flow
            <div className="space-y-8">
                    <div className="grid gap-6 md:grid-cols-2">
                      <Card className="bg-gradient-card border-border/50">
                        <CardHeader>
                          <CardTitle className="flex items-center gap-2">
                            <BookOpen className="h-5 w-5" />
                            New to mirrord?
                          </CardTitle>
                          <CardDescription>
                            Learn how mirrord works and explore the overview before creating your first configuration
                          </CardDescription>
                        </CardHeader>
                        <CardContent>
                          <Button onClick={() => setShowExplanation(true)} className="w-full">
                            Start with Overview
                            <ArrowRight className="h-4 w-4 ml-2" />
                          </Button>
                        </CardContent>
                      </Card>

                      <Card className="bg-gradient-card border-border/50">
                        <CardHeader>
                          <CardTitle className="flex items-center gap-2">
                            <Zap className="h-5 w-5" />
                            Ready to Configure?
                          </CardTitle>
                          <CardDescription>
                            Skip the overview and jump directly to creating your mirrord.json configuration
                          </CardDescription>
                        </CardHeader>
                        <CardContent>
                          <Button variant="outline" onClick={() => setShowWizard(true)} className="w-full">
                            Create Configuration
                            <Plus className="h-4 w-4 ml-2" />
                          </Button>
                        </CardContent>
                      </Card>
                    </div>

                {showExplanation && <div className="space-y-6 mt-8">
                    <Card className="bg-gradient-card border-border/50">
                      <CardHeader>
                        <CardTitle>What is mirrord?</CardTitle>
                        <CardDescription>
                          Revolutionary Kubernetes development without deployment overhead
                        </CardDescription>
                      </CardHeader>
                      <CardContent className="space-y-6">
                        <div className="space-y-4">
                          <p className="text-foreground">
                            mirrord lets you run your local code in the context of your cloud environment. Unlike traditional development tools, you can test your code with real cloud dependencies, data, and configuration without deploying anything.
                          </p>
                          
                          <div className="bg-muted/30 p-4 rounded-lg">
                            <h4 className="font-medium mb-2 text-accent-foreground">Why mirrord vs. Other Solutions?</h4>
                            <div className="grid gap-3 text-sm">
                              <div className="flex gap-2">
                                <span className="text-red-500">×</span>
                                <span className="text-muted-foreground"><strong>Port forwarding:</strong> Only gives you database access, no real environment context</span>
                              </div>
                              <div className="flex gap-2">
                                <span className="text-red-500">×</span>
                                <span className="text-muted-foreground"><strong>Local clusters:</strong> Resource-heavy, doesn't match production environment</span>
                              </div>
                              <div className="flex gap-2">
                                <span className="text-red-500">×</span>
                                <span className="text-muted-foreground"><strong>Deploy to dev cluster:</strong> Slow feedback loop, complex CI/CD setup</span>
                              </div>
                              <div className="flex gap-2">
                                <span className="text-green-500">✓</span>
                                <span className="text-foreground"><strong>mirrord:</strong> Run locally with full cloud context in seconds</span>
                              </div>
                            </div>
                          </div>
                        </div>

                        <div className="space-y-4">
                          <h4 className="font-medium text-accent-foreground">How mirrord Works</h4>
                          <p className="text-sm text-muted-foreground">
                            mirrord runs in two places - in the memory of your local process (mirrord-layer), and as a pod in your cloud environment (mirrord-agent).
                          </p>
                          
                          <div className="bg-muted/30 p-4 rounded-lg">
                            <h5 className="font-medium mb-3 text-accent-foreground">mirrord - Basic Architecture</h5>
                            <div className="space-y-3 text-sm">
                              <p className="text-muted-foreground">
                                When you start your local process with mirrord, it creates a pod in your cloud environment, which listens in on the pod you've passed as an argument. mirrord-layer then does the following:
                              </p>
                              
                              <div className="space-y-2 ml-4">
                                <p className="text-muted-foreground"><strong>Override the process' syscalls to:</strong></p>
                                <ul className="space-y-1 ml-4 text-muted-foreground">
                                  <li>• Listen to incoming traffic from the agent, instead of local sockets.</li>
                                  <li>• Intercept outgoing traffic and send it out from the remote pod, instead of locally.</li>
                                  <li>• Read and write files to the remote file system.</li>
                                  <li>• Merge the process' environment variables with those of the remote pod.</li>
                                </ul>
                              </div>
                              
                              <p className="text-muted-foreground">
                                The remote part of this logic is handled by the agent, which runs in the network namespace of the remote pod, and can access its file system and environment variables.
                              </p>
                            </div>
                            
                            <div className="mt-4">
                              <img 
                                src={mirroredArchitecture} 
                                alt="mirrord Architecture Diagram" 
                                className="w-full max-w-2xl mx-auto rounded-lg border border-border/20"
                              />
                            </div>
                          </div>
                        </div>

                        <div className="space-y-4">
                          <h4 className="font-medium text-accent-foreground">How mirrord Works - Two Key Modes:</h4>
                          <div className="grid gap-4">
                            <div className="flex gap-3 p-4 rounded-lg bg-muted/30">
                              <div className="w-2 h-2 rounded-full bg-blue-500 mt-2"></div>
                              <div>
                                <h4 className="font-medium mb-1">Mirroring</h4>
                                <p className="text-sm text-muted-foreground">Your local service receives a copy of the traffic while the remote service continues to handle requests normally. Perfect for testing with real traffic without disruption.</p>
                              </div>
                            </div>
                            <div className="flex gap-3 p-4 rounded-lg bg-muted/30">
                              <div className="w-2 h-2 rounded-full bg-green-500 mt-2"></div>
                              <div>
                                <h4 className="font-medium mb-1">Stealing</h4>
                                <p className="text-sm text-muted-foreground">Your local service completely replaces the remote service. All traffic is redirected to your local code, giving you full control for development and testing.</p>
                              </div>
                            </div>
                          </div>
                        </div>
                        <div className="flex gap-3 pt-4">
                          <Button onClick={() => {
                        setShowExplanation(false);
                        setShowWizard(true);
                      }} className="bg-gradient-primary">
                            <ArrowRight className="h-4 w-4 mr-2" />
                            Get Started
                          </Button>
                          
                        </div>
                      </CardContent>
                    </Card>
                  </div>}

                {(showOnboarding || showWizard) && <ConfigWizard isOpen={showOnboarding || showWizard} onClose={() => {
                setShowOnboarding(false);
                setShowWizard(false);
              }} onSave={config => {
                const newConfig = {
                  ...config,
                  id: Date.now().toString(),
                  service: config.target?.split(' ')[0] || 'my-service',
                  createdAt: new Date().toISOString().split('T')[0]
                };
                const serviceName = newConfig.service;
                const existingServiceIndex = services.findIndex(s => s.name === serviceName);
                if (existingServiceIndex >= 0) {
                  setServices(services.map((service, index) => {
                    if (index === existingServiceIndex) {
                      return {
                        ...service,
                        configs: [...service.configs, newConfig]
                      };
                    }
                    return service;
                  }));
                } else {
                  setServices([...services, {
                    name: serviceName,
                    configs: [newConfig]
                  }]);
                }
                setShowOnboarding(false);
                setShowWizard(false);
                setActiveSection("configs");
              }} existingConfigs={services.flatMap(s => s.configs)} />}
                  </div> :
            // Returning user flow
            <Card className="bg-gradient-card border-border/50 shadow-glow">
                    <CardHeader>
                      <CardTitle className="flex items-center gap-2">
                        <Plus className="h-5 w-5" />
                        Create Configuration
                      </CardTitle>
                      <CardDescription>
                        Use our wizard to create a working mirrord.json config without manual editing
                      </CardDescription>
                    </CardHeader>
                    <CardContent>
                      <div className="flex gap-3">
                        <Button onClick={() => setShowWizard(true)} className="bg-violet-600 hover:bg-violet-700 text-white transition-all duration-300 px-6 py-3 h-auto">
                          <Plus className="h-4 w-4 mr-2" />
                          Create
                        </Button>
                        <Button variant="outline" className="border-accent text-accent-foreground hover:bg-accent/20 transition-all duration-300 px-6 py-3 h-auto">
                          Learn More About mirrord
                        </Button>
                      </div>
                    </CardContent>
                  </Card>}
              </div>}

            {activeSection === "configs" && <div className="max-w-6xl">
                <div className="flex items-center justify-between mb-6">
                  <div>
                    <h1 className="text-3xl font-bold mb-2">Configuration Files</h1>
                    <p className="text-muted-foreground">
                      Manage your mirrord.json configurations
                    </p>
                  </div>
                  <Button onClick={() => setShowWizard(true)} className="bg-gradient-primary hover:shadow-glow transition-all duration-300">
                    <Plus className="h-4 w-4 mr-2" />
                    New Config
                  </Button>
                </div>

                <div className="grid gap-6">
                  {services.map((service, serviceIndex) => <div key={service.name} className="space-y-4">
                      <div className="flex items-center gap-3">
                        <h2 className="text-xl font-semibold">{service.name}</h2>
                        <Badge variant="secondary" className="text-xs">
                          {service.configs.length} config{service.configs.length !== 1 ? 's' : ''}
                        </Badge>
                      </div>
                      
                      <div className="grid gap-3 ml-4">
                        {service.configs.map(config => <Card key={config.id} className={`transition-all duration-300 ${config.isActive ? 'bg-gradient-card border-primary/50 shadow-glow' : 'bg-card hover:bg-gradient-card/50'}`}>
                            <CardContent className="p-4">
                              <div className="flex items-center justify-between">
                                <div className="flex-1">
                                  <div className="flex items-center gap-3 mb-2">
                                    <h3 className="text-lg font-medium">{config.name}</h3>
                                    {config.isActive && <Badge className="bg-primary text-primary-foreground text-xs">
                                        ACTIVE
                                      </Badge>}
                                  </div>
                                  <p className="text-muted-foreground text-sm mb-3">
                                    Target: {config.target}
                                  </p>
                                  <div className="flex items-center gap-4 text-xs text-muted-foreground">
                                    <span>Created: {config.createdAt}</span>
                                    <div className="flex items-center gap-1">
                                      {config.fileSystem.enabled && <Badge variant="outline" className="text-xs">FS</Badge>}
                                      {(config.network.incoming.enabled || config.network.outgoing.enabled || config.network.dns.enabled) && <Badge variant="outline" className="text-xs">NET</Badge>}
                                      {config.environment.enabled && <Badge variant="outline" className="text-xs">ENV</Badge>}
                                    </div>
                                  </div>
                                </div>
                                <div className="flex items-center gap-2">
                                  {!config.isActive && <Button variant="outline" size="sm" onClick={() => handleSetActive(serviceIndex, config.id)}>
                                      <Save className="h-4 w-4 mr-2" />
                                      Set Active
                                    </Button>}
                                  <Button variant="outline" size="sm">
                                    <Edit3 className="h-4 w-4" />
                                  </Button>
                                  <Button variant="outline" size="sm" onClick={() => handleDuplicate(serviceIndex, config.id)}>
                                    <Copy className="h-4 w-4" />
                                  </Button>
                                  <Button variant="outline" size="sm" onClick={() => handleDelete(serviceIndex, config.id)} className="text-destructive hover:text-destructive-foreground hover:bg-destructive">
                                    <Trash2 className="h-4 w-4" />
                                  </Button>
                                </div>
                              </div>
                            </CardContent>
                          </Card>)}
                      </div>
                    </div>)}
                </div>
              </div>}
          </main>
        </div>

      </div>
    </SidebarProvider>;
}