import { useState, useEffect } from "react";
import { Settings, Plus, FileCode2, Zap, Save, Edit3, Copy, Trash2, Home, BookOpen, ArrowRight, BarChart3 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { SidebarProvider, Sidebar, SidebarContent, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarMenu, SidebarMenuItem, SidebarMenuButton, SidebarTrigger } from "@/components/ui/sidebar";
import { ConfigWizard } from "@/components/ConfigWizard";
import { UtilizationDashboard } from "@/components/utilization/UtilizationDashboard";

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

const Dashboard = () => {
  const [showWizard, setShowWizard] = useState(false);
  const [wizardMode, setWizardMode] = useState<'create' | 'overview'>('create');
  const [services, setServices] = useState<Service[]>([]);
  const [activeSection, setActiveSection] = useState("home");

  useEffect(() => {
    // Load configs from localStorage
    const savedConfigs = JSON.parse(localStorage.getItem('mirrord-configs') || '[]');
    // Group configs by service
    const groupedServices: Service[] = [];
    savedConfigs.forEach((config: Config) => {
      const existingService = groupedServices.find(s => s.name === config.service);
      if (existingService) {
        existingService.configs.push(config);
      } else {
        groupedServices.push({
          name: config.service,
          configs: [config]
        });
      }
    });
    setServices(groupedServices);
  }, []);

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
    const updatedServices = services.map((service, sIndex) => {
      if (sIndex === serviceIndex) {
        return {
          ...service,
          configs: service.configs.filter(config => config.id !== configId)
        };
      }
      return service;
    }).filter(service => service.configs.length > 0);

    setServices(updatedServices);
    
    // Update localStorage
    const allConfigs = updatedServices.flatMap(s => s.configs);
    localStorage.setItem('mirrord-configs', JSON.stringify(allConfigs));
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
      
      const updatedServices = services.map((s, sIndex) => {
        if (sIndex === serviceIndex) {
          return {
            ...s,
            configs: [...s.configs, newConfig]
          };
        }
        return s;
      });
      
      setServices(updatedServices);
      
      // Update localStorage
      const allConfigs = updatedServices.flatMap(s => s.configs);
      localStorage.setItem('mirrord-configs', JSON.stringify(allConfigs));
    }
  };

  const handleConfigSave = (config: Partial<Config>) => {
    const newConfig = {
      ...config,
      id: Date.now().toString(),
      service: config.target?.split(' ')[0] || 'my-service',
      createdAt: new Date().toISOString().split('T')[0]
    };
    
    const serviceName = newConfig.service;
    const existingServiceIndex = services.findIndex(s => s.name === serviceName);
    
    let updatedServices;
    if (existingServiceIndex >= 0) {
      updatedServices = services.map((service, index) => {
        if (index === existingServiceIndex) {
          return {
            ...service,
            configs: [...service.configs, newConfig as Config]
          };
        }
        return service;
      });
    } else {
      updatedServices = [...services, {
        name: serviceName,
        configs: [newConfig as Config]
      }];
    }
    
    setServices(updatedServices);
    
    // Update localStorage
    const allConfigs = updatedServices.flatMap(s => s.configs);
    localStorage.setItem('mirrord-configs', JSON.stringify(allConfigs));
    
    setShowWizard(false);
  };

  return (
    <SidebarProvider>
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
                      <SidebarMenuButton isActive={activeSection === "home"} onClick={() => setActiveSection("home")}>
                        <Home className="h-4 w-4" />
                        <span>Home</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                    <SidebarMenuItem>
                      <SidebarMenuButton isActive={activeSection === "configs"} onClick={() => setActiveSection("configs")}>
                        <FileCode2 className="h-4 w-4" />
                        <span>Config Files</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                    <SidebarMenuItem>
                      <SidebarMenuButton isActive={activeSection === "utilization"} onClick={() => setActiveSection("utilization")}>
                        <BarChart3 className="h-4 w-4" />
                        <span>Utilization</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
              
              {services.length > 0 && (
                <SidebarGroup>
                  <SidebarGroupLabel>Saved Configurations</SidebarGroupLabel>
                  <SidebarGroupContent>
                    <SidebarMenu>
                      {services.flatMap(service => 
                        service.configs.map(config => (
                          <SidebarMenuItem key={config.id}>
                            <SidebarMenuButton
                              onClick={() => setActiveSection("configs")}
                              className="flex items-center justify-between w-full"
                            >
                              <div className="flex flex-col items-start min-w-0 flex-1">
                                <span className="text-sm font-medium truncate">{config.name}</span>
                                <span className="text-xs text-muted-foreground truncate">{service.name}</span>
                              </div>
                              {config.isActive && (
                                <Badge className="bg-primary text-primary-foreground text-xs ml-2">
                                  Active
                                </Badge>
                              )}
                            </SidebarMenuButton>
                          </SidebarMenuItem>
                        ))
                      )}
                    </SidebarMenu>
                  </SidebarGroupContent>
                </SidebarGroup>
              )}
            </SidebarContent>
          </Sidebar>

          <main className="flex-1">
            {activeSection === "home" && (
              <div className="min-h-screen w-full bg-background flex items-center justify-center p-6">
                <div className="max-w-4xl mx-auto">
                  <div className="text-center mb-8 sm:mb-12">
                    <h1 className="text-2xl sm:text-3xl font-bold mb-4 text-foreground">
                      Welcome back! 👋
                    </h1>
                    <p className="text-muted-foreground text-sm sm:text-lg max-w-2xl mx-auto">
                      Ready to create your next mirrord configuration or learn more about the platform?
                    </p>
                  </div>

                  <div className="grid gap-6 sm:gap-8 md:grid-cols-2 max-w-4xl mx-auto">
                    <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
                      <CardHeader>
                        <CardTitle className="flex items-center gap-2">
                          <Plus className="h-5 w-5" />
                          Create Configuration
                        </CardTitle>
                        <CardDescription>
                          Use our wizard to create a working mirrord.json configuration file for your project
                        </CardDescription>
                      </CardHeader>
                      <CardContent>
                        <Button onClick={() => {
                          setWizardMode('create');
                          setShowWizard(true);
                        }} className="w-full bg-gradient-primary hover:shadow-glow">
                          Create New Config
                          <ArrowRight className="h-4 w-4 ml-2" />
                        </Button>
                      </CardContent>
                    </Card>

                    <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
                      <CardHeader>
                        <CardTitle className="flex items-center gap-2">
                          <BookOpen className="h-5 w-5" />
                          Learn About mirrord
                        </CardTitle>
                        <CardDescription>
                          Explore how mirrord works and understand the different modes and configurations
                        </CardDescription>
                      </CardHeader>
                      <CardContent>
                        <Button variant="outline" onClick={() => {
                          setWizardMode('overview');
                          setShowWizard(true);
                        }} className="w-full">
                          View Overview
                          <ArrowRight className="h-4 w-4 ml-2" />
                        </Button>
                      </CardContent>
                    </Card>
                  </div>

                  {services.length > 0 && (
                    <div className="mt-12">
                      <div className="text-center mb-6">
                        <h2 className="text-xl font-semibold mb-2">Your Configurations</h2>
                        <p className="text-muted-foreground text-sm">
                          You have {services.length} service{services.length !== 1 ? 's' : ''} with configurations
                        </p>
                      </div>
                      <div className="flex justify-center">
                        <Button variant="outline" onClick={() => setActiveSection("configs")}>
                          <FileCode2 className="h-4 w-4 mr-2" />
                          Manage Config Files
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeSection === "overview" && (
              <div className="p-6">
                <div className="max-w-4xl mx-auto">
                  <div className="mb-8">
                    <h1 className="text-3xl font-bold mb-2">mirrord Overview</h1>
                    <p className="text-muted-foreground">
                      Learn how mirrord works and explore its capabilities
                    </p>
                  </div>
                  
                  <div className="grid gap-6">
                    <Card className="bg-gradient-card border-border/50">
                      <CardHeader>
                        <CardTitle>What is mirrord?</CardTitle>
                      </CardHeader>
                      <CardContent>
                        <p className="text-muted-foreground">
                          mirrord lets you run your local code in the context of your cloud environment. Unlike traditional development tools, you can test your code with real cloud dependencies, data, and configuration without deploying anything.
                        </p>
                      </CardContent>
                    </Card>

                    <div className="grid md:grid-cols-3 gap-4">
                      <Card className="bg-gradient-card border-border/50">
                        <CardHeader>
                          <CardTitle className="text-lg">Filtering Mode</CardTitle>
                        </CardHeader>
                        <CardContent>
                          <p className="text-sm text-muted-foreground">
                            Your local service only handles matching traffic based on HTTP Headers/Path. This allows you to control what flows in the system you want to affect.
                          </p>
                        </CardContent>
                      </Card>

                      <Card className="bg-gradient-card border-border/50">
                        <CardHeader>
                          <CardTitle className="text-lg">Mirror Mode</CardTitle>
                        </CardHeader>
                        <CardContent>
                          <p className="text-sm text-muted-foreground">
                            Your local service receives a copy of the incoming traffic. Best for debugging existing traffic without disrupting the environment.
                          </p>
                        </CardContent>
                      </Card>

                      <Card className="bg-gradient-card border-border/50">
                        <CardHeader>
                          <CardTitle className="text-lg">Replace Mode</CardTitle>
                        </CardHeader>
                        <CardContent>
                          <p className="text-sm text-muted-foreground">
                            Completely substitute the remote service. Useful when you want to work against queues and have full control.
                          </p>
                        </CardContent>
                      </Card>
                    </div>

                    <div className="flex justify-center mt-8">
                      <Button onClick={() => {
                        setWizardMode('create');
                        setShowWizard(true);
                      }} className="bg-gradient-primary hover:shadow-glow">
                        <Plus className="h-4 w-4 mr-2" />
                        Create Your Configuration
                      </Button>
                    </div>
                  </div>
                </div>
              </div>
            )}

            {activeSection === "configs" && (
              <div className="p-6">
                <div className="max-w-6xl">
                  <div className="flex items-center justify-between mb-6">
                    <div>
                      <h1 className="text-3xl font-bold mb-2">Configuration Files</h1>
                      <p className="text-muted-foreground">
                        Manage your mirrord.json configurations
                      </p>
                    </div>
                    <Button onClick={() => {
                      setWizardMode('create');
                      setShowWizard(true);
                    }} className="bg-gradient-primary hover:shadow-glow transition-all duration-300">
                      <Plus className="h-4 w-4 mr-2" />
                      New Config
                    </Button>
                  </div>

                  {services.length === 0 ? (
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
                        <Button onClick={() => {
                          setWizardMode('create');
                          setShowWizard(true);
                        }} className="bg-gradient-primary">
                          <Plus className="h-4 w-4 mr-2" />
                          Create Your First Config
                        </Button>
                      </CardContent>
                    </Card>
                  ) : (
                    <div className="grid gap-6">
                      {services.map((service, serviceIndex) => (
                        <div key={service.name} className="space-y-4">
                          <div className="flex items-center gap-3">
                            <h2 className="text-xl font-semibold">{service.name}</h2>
                            <Badge variant="secondary" className="text-xs">
                              {service.configs.length} config{service.configs.length !== 1 ? 's' : ''}
                            </Badge>
                          </div>
                          
                          <div className="grid gap-3 ml-4">
                            {service.configs.map(config => (
                              <Card key={config.id} className={`transition-all duration-300 ${config.isActive ? 'bg-gradient-card border-primary/50 shadow-glow' : 'bg-card hover:bg-gradient-card/50'}`}>
                                <CardContent className="p-4">
                                  <div className="flex items-center justify-between">
                                    <div className="flex-1">
                                      <div className="flex items-center gap-3 mb-2">
                                        <h3 className="text-lg font-medium">{config.name}</h3>
                                        {config.isActive && (
                                          <Badge className="bg-primary text-primary-foreground text-xs">
                                            ACTIVE
                                          </Badge>
                                        )}
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
                                      {!config.isActive && (
                                        <Button variant="outline" size="sm" onClick={() => handleSetActive(serviceIndex, config.id)}>
                                          <Save className="h-4 w-4 mr-2" />
                                          Set Active
                                        </Button>
                                      )}
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
                              </Card>
                            ))}
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeSection === "utilization" && (
              <UtilizationDashboard />
            )}
          </main>
        </div>

        {showWizard && (
          <ConfigWizard 
            isOpen={showWizard} 
            onClose={() => setShowWizard(false)} 
            onSave={handleConfigSave}
            existingConfigs={services.flatMap(s => s.configs)}
            mode={wizardMode}
          />
        )}
      </div>
    </SidebarProvider>
  );
};

export default Dashboard;