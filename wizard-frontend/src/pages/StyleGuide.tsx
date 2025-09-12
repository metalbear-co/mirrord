import { Server, Network, HardDrive, Settings2, Save, Plus, Trash2, ChevronDown } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Checkbox } from "@/components/ui/checkbox";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

export default function StyleGuide() {
  return (
    <div className="min-h-screen bg-background p-8">
      <div className="max-w-6xl mx-auto space-y-8">
        {/* Header */}
        <div className="text-center space-y-4">
          <h1 className="text-4xl font-bold text-foreground">mirrord UI Style Guide</h1>
          <p className="text-lg text-muted-foreground">
            Complete visual reference for the mirrord configuration system
          </p>
        </div>

        {/* Color Palette */}
        <Card>
          <CardHeader>
            <CardTitle>Color Palette</CardTitle>
            <CardDescription>All colors use HSL values and semantic tokens</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
              <div className="space-y-2">
                <div className="h-16 bg-primary rounded-lg border"></div>
                <div className="text-sm">
                  <div className="font-medium">Primary</div>
                  <div className="text-xs text-muted-foreground">hsl(258 90% 66%)</div>
                </div>
              </div>
              <div className="space-y-2">
                <div className="h-16 bg-secondary rounded-lg border"></div>
                <div className="text-sm">
                  <div className="font-medium">Secondary</div>
                  <div className="text-xs text-muted-foreground">hsl(240 4% 88%)</div>
                </div>
              </div>
              <div className="space-y-2">
                <div className="h-16 bg-muted rounded-lg border"></div>
                <div className="text-sm">
                  <div className="font-medium">Muted</div>
                  <div className="text-xs text-muted-foreground">hsl(240 4% 88%)</div>
                </div>
              </div>
              <div className="space-y-2">
                <div className="h-16 bg-accent rounded-lg border"></div>
                <div className="text-sm">
                  <div className="font-medium">Accent</div>
                  <div className="text-xs text-muted-foreground">hsl(240 4% 60%)</div>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Typography */}
        <Card>
          <CardHeader>
            <CardTitle>Typography</CardTitle>
            <CardDescription>Text styles and hierarchy</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div>
              <h1 className="text-4xl font-bold text-foreground">Heading 1 - Configuration Title</h1>
              <p className="text-sm text-muted-foreground">text-4xl font-bold</p>
            </div>
            <div>
              <h2 className="text-2xl font-semibold text-foreground">Heading 2 - Section Title</h2>
              <p className="text-sm text-muted-foreground">text-2xl font-semibold</p>
            </div>
            <div>
              <h3 className="text-lg font-medium text-foreground">Heading 3 - Card Title</h3>
              <p className="text-sm text-muted-foreground">text-lg font-medium</p>
            </div>
            <div>
              <p className="text-base text-foreground">Body Text - Regular content and descriptions</p>
              <p className="text-sm text-muted-foreground">text-base</p>
            </div>
            <div>
              <p className="text-sm text-muted-foreground">Small Text - Hints and supplementary information</p>
              <p className="text-xs text-muted-foreground">text-sm text-muted-foreground</p>
            </div>
            <div>
              <p className="text-xs text-muted-foreground/60">Micro Text - Tooltips and very minor details</p>
              <p className="text-xs text-muted-foreground">text-xs text-muted-foreground/60</p>
            </div>
          </CardContent>
        </Card>

        {/* Form Components */}
        <Card>
          <CardHeader>
            <CardTitle>Form Components</CardTitle>
            <CardDescription>Input fields, labels, and form controls</CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            {/* Text Input */}
            <div className="space-y-2">
              <Label htmlFor="demo-input">Configuration Name</Label>
              <Input
                id="demo-input"
                placeholder="e.g., prod-api-config"
                value="my-mirrord-config"
              />
              <p className="text-xs text-muted-foreground/60 mt-1">
                💡 Used to identify this configuration in IDE extensions and CLI tools
              </p>
            </div>

            {/* Select Dropdown */}
            <div className="space-y-2">
              <Label>Namespace</Label>
              <Select value="default">
                <SelectTrigger>
                  <SelectValue placeholder="Select a namespace" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="default">default</SelectItem>
                  <SelectItem value="production">production</SelectItem>
                  <SelectItem value="staging">staging</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Target with Description */}
            <div className="space-y-2">
              <Label htmlFor="target">Target</Label>
              <p className="text-xs text-muted-foreground mb-2">
                The remote process inside your Kubernetes cluster that your local process will "impersonate" to.
              </p>
              <Popover>
                <PopoverTrigger asChild>
                  <Button
                    variant="outline"
                    role="combobox"
                    className="w-full justify-between"
                  >
                    api-service
                    <ChevronDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
                  </Button>
                </PopoverTrigger>
                <PopoverContent className="w-[400px] p-0 bg-background border z-50">
                  <div className="p-3">
                    <Input
                      placeholder="e.g., search or type target name..."
                      className="mb-2"
                    />
                    <div className="space-y-1">
                      <div className="flex items-center gap-2 p-2 hover:bg-muted cursor-pointer rounded text-sm">
                        <span>api-service</span>
                        <Badge variant="outline" className="text-xs">deployment</Badge>
                      </div>
                      <div className="flex items-center gap-2 p-2 hover:bg-muted cursor-pointer rounded text-sm">
                        <span>frontend-app</span>
                        <Badge variant="outline" className="text-xs">deployment</Badge>
                      </div>
                    </div>
                  </div>
                </PopoverContent>
              </Popover>
            </div>

            {/* Textarea */}
            <div className="space-y-2">
              <Label htmlFor="filter">Filter Expression</Label>
              <Textarea
                id="filter"
                placeholder="e.g., /api/v1/*"
                className="min-h-[80px]"
              />
              <p className="text-xs text-muted-foreground/60">
                Use glob patterns or regex to define filtering rules
              </p>
            </div>

            {/* Switch with Description */}
            <div className="flex items-center justify-between p-4 bg-muted/30 rounded-lg">
              <div>
                <Label>Enable Network Mirroring</Label>
                <p className="text-sm text-muted-foreground">
                  Mirror or steal incoming requests from the target
                </p>
              </div>
              <Switch checked={true} />
            </div>

            {/* Checkbox */}
            <div className="flex items-center space-x-2">
              <Checkbox id="advanced" checked />
              <Label htmlFor="advanced" className="text-sm font-medium">
                Enable advanced configuration options
              </Label>
            </div>
          </CardContent>
        </Card>

        {/* Buttons */}
        <Card>
          <CardHeader>
            <CardTitle>Buttons</CardTitle>
            <CardDescription>Button variants and states</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
              <div className="space-y-2">
                <Button className="w-full">Primary</Button>
                <p className="text-xs text-muted-foreground">default variant</p>
              </div>
              <div className="space-y-2">
                <Button variant="secondary" className="w-full">Secondary</Button>
                <p className="text-xs text-muted-foreground">secondary variant</p>
              </div>
              <div className="space-y-2">
                <Button variant="outline" className="w-full">Outline</Button>
                <p className="text-xs text-muted-foreground">outline variant</p>
              </div>
              <div className="space-y-2">
                <Button variant="ghost" className="w-full">Ghost</Button>
                <p className="text-xs text-muted-foreground">ghost variant</p>
              </div>
              <div className="space-y-2">
                <Button variant="destructive" className="w-full">Destructive</Button>
                <p className="text-xs text-muted-foreground">destructive variant</p>
              </div>
              <div className="space-y-2">
                <Button size="sm" className="w-full">
                  <Plus className="h-4 w-4 mr-1" />
                  Small
                </Button>
                <p className="text-xs text-muted-foreground">small size with icon</p>
              </div>
              <div className="space-y-2">
                <Button size="lg" className="w-full">Large</Button>
                <p className="text-xs text-muted-foreground">large size</p>
              </div>
              <div className="space-y-2">
                <Button disabled className="w-full">Disabled</Button>
                <p className="text-xs text-muted-foreground">disabled state</p>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Cards and Layout */}
        <Card>
          <CardHeader>
            <CardTitle>Cards & Layout Components</CardTitle>
            <CardDescription>Card styles and layout patterns</CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            {/* Basic Card */}
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <Server className="h-5 w-5" />
                  Section Card Title
                </CardTitle>
                <CardDescription>
                  This is a card description that explains what this section contains
                </CardDescription>
              </CardHeader>
              <CardContent>
                <p className="text-sm text-muted-foreground">Card content goes here</p>
              </CardContent>
            </Card>

            {/* Tabs */}
            <Tabs defaultValue="tab1" className="w-full">
              <TabsList className="grid w-full grid-cols-3">
                <TabsTrigger value="tab1">Network</TabsTrigger>
                <TabsTrigger value="tab2">File System</TabsTrigger>
                <TabsTrigger value="tab3">Environment</TabsTrigger>
              </TabsList>
              <TabsContent value="tab1" className="space-y-4">
                <p className="text-sm text-muted-foreground">Network configuration content</p>
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>

        {/* Badges and Status */}
        <Card>
          <CardHeader>
            <CardTitle>Badges & Status Indicators</CardTitle>
            <CardDescription>Status badges and indicators</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="flex flex-wrap gap-2">
              <Badge>default</Badge>
              <Badge variant="secondary">secondary</Badge>
              <Badge variant="destructive">destructive</Badge>
              <Badge variant="outline">outline</Badge>
              <Badge className="bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-100">
                active
              </Badge>
              <Badge className="bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-100">
                pending
              </Badge>
            </div>
          </CardContent>
        </Card>

        {/* Progress Steps */}
        <Card>
          <CardHeader>
            <CardTitle>Progress Steps</CardTitle>
            <CardDescription>Step indicators for wizards</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              {[
                { title: "Basic Info", icon: Server, active: false, completed: true },
                { title: "Network", icon: Network, active: true, completed: false },
                { title: "File System", icon: HardDrive, active: false, completed: false },
                { title: "Review", icon: Save, active: false, completed: false }
              ].map((step, index) => (
                <div key={index} className="flex items-center">
                  <div className={`flex items-center gap-2 px-3 py-2 rounded-lg transition-all ${
                    step.active 
                      ? 'bg-primary text-primary-foreground' 
                      : step.completed 
                        ? 'bg-muted text-muted-foreground' 
                        : 'bg-secondary text-secondary-foreground'
                  }`}>
                    <step.icon className="h-4 w-4" />
                    <span className="text-sm font-medium">{step.title}</span>
                  </div>
                  {index < 3 && (
                    <div className="h-4 w-4 text-muted-foreground mx-2">→</div>
                  )}
                </div>
              ))}
            </div>
          </CardContent>
        </Card>

        {/* Spacing & Sizing */}
        <Card>
          <CardHeader>
            <CardTitle>Spacing & Sizing Guidelines</CardTitle>
            <CardDescription>Standard spacing and sizing patterns</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div>
              <Label>Form Field Spacing</Label>
              <p className="text-xs text-muted-foreground mb-2">space-y-4 between form fields</p>
              <div className="bg-muted/20 p-4 rounded border-2 border-dashed border-muted">
                Form content area
              </div>
            </div>
            <div>
              <Label>Card Content Padding</Label>
              <p className="text-xs text-muted-foreground mb-2">p-6 for card content</p>
              <div className="bg-muted/20 p-6 rounded border-2 border-dashed border-muted">
                Card content with standard padding
              </div>
            </div>
            <div>
              <Label>Button Heights</Label>
              <div className="flex items-center gap-2 mt-2">
                <Button size="sm">Small (32px)</Button>
                <Button>Default (40px)</Button>
                <Button size="lg">Large (44px)</Button>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Separators */}
        <Card>
          <CardHeader>
            <CardTitle>Separators & Dividers</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div>
              <p className="text-sm mb-2">Horizontal separator</p>
              <Separator />
            </div>
            <div className="flex items-center gap-4">
              <p className="text-sm">Vertical separator</p>
              <Separator orientation="vertical" className="h-6" />
              <p className="text-sm">Between items</p>
            </div>
          </CardContent>
        </Card>

        {/* Footer */}
        <div className="text-center text-sm text-muted-foreground py-8">
          This style guide reflects the current design system. All components use semantic tokens from index.css.
        </div>
      </div>
    </div>
  );
}